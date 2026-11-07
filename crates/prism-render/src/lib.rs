//! Deterministic render-graph dependency planning.
//!
//! Nodes contain only dependency metadata in this crate. Resource allocation,
//! shader compilation, and CPU/GPU execution are backend responsibilities.

use prism_compositor::{
    BlendMode, CompositorError, Rgba, TransitionKind, composite_over, transition_pixel,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use thiserror::Error;

/// Stable identifier for a node in one render graph.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(u32);

impl NodeId {
    /// Returns the numeric identifier, primarily for diagnostics.
    #[must_use]
    pub fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "node-{}", self.0)
    }
}

/// Semantic operation represented by a render node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    /// Decoded media frame source.
    MediaSource,
    /// Geometric transform such as position, scale, or rotation.
    Transform,
    /// Crop or rectangular mask operation.
    Crop,
    /// Color-space or grading operation.
    Color,
    /// Video or audio effect operation.
    Effect,
    /// Layer blend or alpha-composite operation.
    Blend,
    /// Transition between adjacent inputs.
    Transition,
    /// Rasterized title or caption.
    Text,
    /// Final sequence output.
    Output,
}

/// Typed render node metadata consumed by a backend executor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderNode {
    /// Stable graph identifier.
    pub id: NodeId,
    /// Semantic operation represented by this node.
    pub kind: NodeKind,
    /// Input nodes, in operation-defined order.
    pub inputs: Vec<NodeId>,
}

/// Render graph validation failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderGraphError {
    /// A node referred to an input not present in the graph.
    MissingInput {
        /// Node containing the invalid reference.
        node: NodeId,
        /// Referenced node that is absent.
        input: NodeId,
    },
    /// A node listed one input more than once.
    DuplicateInput {
        /// Node containing the duplicate reference.
        node: NodeId,
        /// Repeated input identifier.
        input: NodeId,
    },
    /// A node kind received an invalid number of inputs.
    InvalidInputArity {
        /// Node being created.
        node: NodeId,
        /// Semantic kind that defines the arity contract.
        kind: NodeKind,
        /// Number of inputs supplied by the caller.
        actual: usize,
    },
    /// Dependencies contain a cycle and cannot be evaluated.
    Cycle,
}

impl fmt::Display for RenderGraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingInput { node, input } => {
                write!(formatter, "{node} references missing input {input}")
            }
            Self::DuplicateInput { node, input } => {
                write!(formatter, "{node} lists input {input} more than once")
            }
            Self::InvalidInputArity { node, kind, actual } => {
                write!(
                    formatter,
                    "{node} of kind {kind:?} received {actual} input(s)"
                )
            }
            Self::Cycle => formatter.write_str("render graph contains a cycle"),
        }
    }
}

impl std::error::Error for RenderGraphError {}

/// Dependency-only render graph with deterministic node identifiers.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderGraph {
    nodes: BTreeMap<NodeId, RenderNode>,
    next_id: u32,
}

/// CPU execution failures for the currently supported render-node subset.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CpuRenderError {
    /// Graph dependency validation failed before execution.
    #[error("render graph validation failed: {0}")]
    Graph(#[from] RenderGraphError),
    /// A media source node had no caller-supplied pixel.
    #[error("media source {0} has no supplied pixel")]
    MissingSource(NodeId),
    /// A dependency did not produce a pixel value.
    #[error("node {node} has no evaluated input {input}")]
    MissingEvaluatedInput {
        /// Consumer node requesting the value.
        node: NodeId,
        /// Dependency that was not evaluated.
        input: NodeId,
    },
    /// The requested output node is absent.
    #[error("output node does not exist")]
    OutputNotFound,
    /// The requested node is not an output node.
    #[error("requested render node is not an output node")]
    NotOutputNode,
    /// The node kind has no CPU resource operation yet.
    #[error("CPU renderer does not yet support {0:?} nodes")]
    UnsupportedNode(NodeKind),
    /// A compositor operation rejected a pixel.
    #[error("compositor operation failed: {0}")]
    Compositor(#[from] CompositorError),
    /// A supplied CPU operation contained an invalid parameter.
    #[error("invalid CPU operation for node {0}")]
    InvalidOperation(NodeId),
    /// A frame had zero/overflowing dimensions or the wrong pixel count.
    #[error("CPU frame dimensions do not match its pixels")]
    InvalidFrameDimensions,
    /// Source frames in one graph evaluation had different dimensions.
    #[error("source frame {node} dimensions do not match {expected_width}x{expected_height}")]
    SourceFrameSizeMismatch {
        /// Source node with incompatible dimensions.
        node: NodeId,
        /// Required frame width.
        expected_width: usize,
        /// Required frame height.
        expected_height: usize,
    },
}

/// Owned linear-light RGBA CPU frame resource.
#[derive(Clone, Debug, PartialEq)]
pub struct CpuFrame {
    /// Frame width in pixels.
    pub width: usize,
    /// Frame height in pixels.
    pub height: usize,
    /// Row-major straight-alpha pixels.
    pub pixels: Box<[Rgba]>,
}

impl CpuFrame {
    /// Creates a frame after validating dimensions and every pixel.
    ///
    /// # Errors
    ///
    /// Returns [`CpuRenderError::InvalidFrameDimensions`] or
    /// [`CpuRenderError::Compositor`] for malformed input.
    pub fn new(width: usize, height: usize, pixels: Vec<Rgba>) -> Result<Self, CpuRenderError> {
        if width == 0
            || height == 0
            || width
                .checked_mul(height)
                .is_none_or(|count| count != pixels.len())
        {
            return Err(CpuRenderError::InvalidFrameDimensions);
        }
        for &pixel in &pixels {
            pixel.validate()?;
        }
        Ok(Self {
            width,
            height,
            pixels: pixels.into_boxed_slice(),
        })
    }

    /// Returns a pixel by coordinate, or `None` outside the frame.
    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> Option<Rgba> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.pixels.get(y * self.width + x).copied()
    }
}

/// Parameterized operations supported by the CPU pixel executor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CpuPixelOperation {
    /// Multiplies linear RGB channels and clamps to the normalized range.
    ColorGain {
        /// Red multiplier in `[0, 4]`.
        red: f32,
        /// Green multiplier in `[0, 4]`.
        green: f32,
        /// Blue multiplier in `[0, 4]`.
        blue: f32,
    },
    /// Multiplies alpha by a normalized opacity value.
    Opacity(f32),
    /// Selects the compositor equation for a blend node.
    Blend(BlendMode),
}

/// Evaluates one output pixel through the supported CPU render-graph subset.
///
/// `MediaSource` values come from `sources`; `Blend` uses normal source-over
/// unless a [`CpuPixelOperation::Blend`] override is supplied,
/// `Transition` uses a cross-dissolve at `transition_progress`, and `Output`
/// forwards its single input. Parameterized transform/color evaluation is
/// available through [`evaluate_cpu_pixel_with_operations`].
///
/// # Errors
///
/// Returns [`CpuRenderError`] for invalid graph dependencies, missing source
/// pixels, unsupported node kinds, invalid output selection, or compositor
/// validation failures.
pub fn evaluate_cpu_pixel(
    graph: &RenderGraph,
    output: NodeId,
    sources: &BTreeMap<NodeId, Rgba>,
    transition_progress: f32,
) -> Result<Rgba, CpuRenderError> {
    evaluate_cpu_pixel_with_operations(
        graph,
        output,
        sources,
        transition_progress,
        &BTreeMap::new(),
    )
}

/// Evaluates a CPU pixel while applying explicit operations to transform and
/// color nodes. Keeping operations outside graph metadata lets the graph stay
/// serializable while callers provide backend-specific parameters.
///
/// # Errors
///
/// Returns [`CpuRenderError`] for invalid graph dependencies, missing source
/// pixels, unsupported or invalid operations, invalid output selection, or
/// compositor validation failures.
pub fn evaluate_cpu_pixel_with_operations(
    graph: &RenderGraph,
    output: NodeId,
    sources: &BTreeMap<NodeId, Rgba>,
    transition_progress: f32,
    operations: &BTreeMap<NodeId, CpuPixelOperation>,
) -> Result<Rgba, CpuRenderError> {
    let output_node = graph.node(output).ok_or(CpuRenderError::OutputNotFound)?;
    if output_node.kind != NodeKind::Output {
        return Err(CpuRenderError::NotOutputNode);
    }
    let mut values = BTreeMap::new();
    for node_id in graph.evaluation_order()? {
        let node = graph
            .node(node_id)
            .ok_or(CpuRenderError::MissingSource(node_id))?;
        let value = match node.kind {
            NodeKind::MediaSource => sources
                .get(&node_id)
                .copied()
                .ok_or(CpuRenderError::MissingSource(node_id))?,
            NodeKind::Blend => {
                let background = evaluated_input(&values, node_id, node.inputs[0])?;
                let foreground = evaluated_input(&values, node_id, node.inputs[1])?;
                let mode = match operations.get(&node_id).copied() {
                    Some(CpuPixelOperation::Blend(mode)) => mode,
                    Some(_) => return Err(CpuRenderError::InvalidOperation(node_id)),
                    None => BlendMode::Normal,
                };
                composite_over(background, foreground, mode)?
            }
            NodeKind::Transition => {
                let outgoing = evaluated_input(&values, node_id, node.inputs[0])?;
                let incoming = evaluated_input(&values, node_id, node.inputs[1])?;
                transition_pixel(
                    outgoing,
                    incoming,
                    transition_progress,
                    TransitionKind::Dissolve,
                )?
            }
            NodeKind::Transform | NodeKind::Color => {
                let input = evaluated_input(&values, node_id, node.inputs[0])?;
                let Some(operation) = operations.get(&node_id).copied() else {
                    return Err(CpuRenderError::UnsupportedNode(node.kind));
                };
                apply_operation(node_id, input, operation)?
            }
            NodeKind::Output => evaluated_input(&values, node_id, node.inputs[0])?,
            unsupported => return Err(CpuRenderError::UnsupportedNode(unsupported)),
        };
        values.insert(node_id, value);
    }
    values
        .get(&output)
        .copied()
        .ok_or(CpuRenderError::MissingSource(output))
}

/// Evaluates a full-frame CPU graph using default operations.
///
/// # Errors
///
/// Returns [`CpuRenderError`] for graph, source-resource, operation, or
/// compositor failures.
pub fn evaluate_cpu_frame(
    graph: &RenderGraph,
    output: NodeId,
    sources: &BTreeMap<NodeId, CpuFrame>,
    transition_progress: f32,
) -> Result<CpuFrame, CpuRenderError> {
    evaluate_cpu_frame_with_operations(
        graph,
        output,
        sources,
        transition_progress,
        &BTreeMap::new(),
    )
}

/// Evaluates full-frame CPU resources through the supported graph subset.
///
/// All source resources are validated before graph evaluation. Every
/// intermediate frame retains the common source dimensions, and node
/// operations are applied per pixel in stable row-major order.
///
/// # Errors
///
/// Returns [`CpuRenderError`] for invalid output selection, dependencies,
/// source frames, operations, unsupported nodes, or compositor failures.
pub fn evaluate_cpu_frame_with_operations(
    graph: &RenderGraph,
    output: NodeId,
    sources: &BTreeMap<NodeId, CpuFrame>,
    transition_progress: f32,
    operations: &BTreeMap<NodeId, CpuPixelOperation>,
) -> Result<CpuFrame, CpuRenderError> {
    let output_node = graph.node(output).ok_or(CpuRenderError::OutputNotFound)?;
    if output_node.kind != NodeKind::Output {
        return Err(CpuRenderError::NotOutputNode);
    }
    for frame in sources.values() {
        if frame.width == 0
            || frame.height == 0
            || frame
                .width
                .checked_mul(frame.height)
                .is_none_or(|count| count != frame.pixels.len())
        {
            return Err(CpuRenderError::InvalidFrameDimensions);
        }
        for &pixel in &frame.pixels {
            pixel.validate()?;
        }
    }

    let mut dimensions = None;
    let mut values = BTreeMap::<NodeId, CpuFrame>::new();
    for node_id in graph.evaluation_order()? {
        let node = graph
            .node(node_id)
            .ok_or(CpuRenderError::MissingSource(node_id))?;
        let frame = match node.kind {
            NodeKind::MediaSource => {
                let source = sources
                    .get(&node_id)
                    .ok_or(CpuRenderError::MissingSource(node_id))?;
                if let Some((width, height)) = dimensions {
                    if source.width != width || source.height != height {
                        return Err(CpuRenderError::SourceFrameSizeMismatch {
                            node: node_id,
                            expected_width: width,
                            expected_height: height,
                        });
                    }
                } else {
                    dimensions = Some((source.width, source.height));
                }
                source.clone()
            }
            NodeKind::Blend | NodeKind::Transition => {
                let left = evaluated_frame(&values, node_id, node.inputs[0])?;
                let right = evaluated_frame(&values, node_id, node.inputs[1])?;
                let mode = match (node.kind, operations.get(&node_id).copied()) {
                    (NodeKind::Blend, Some(CpuPixelOperation::Blend(mode))) => Some(mode),
                    (NodeKind::Blend, None) => Some(BlendMode::Normal),
                    (NodeKind::Transition, None) => None,
                    _ => return Err(CpuRenderError::InvalidOperation(node_id)),
                };
                let pixels = left
                    .pixels
                    .iter()
                    .copied()
                    .zip(right.pixels.iter().copied())
                    .map(|(left, right)| match mode {
                        Some(mode) => composite_over(left, right, mode),
                        None => transition_pixel(
                            left,
                            right,
                            transition_progress,
                            TransitionKind::Dissolve,
                        ),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                CpuFrame::new(left.width, left.height, pixels)?
            }
            NodeKind::Transform | NodeKind::Color => {
                let input = evaluated_frame(&values, node_id, node.inputs[0])?;
                let operation = operations
                    .get(&node_id)
                    .copied()
                    .ok_or(CpuRenderError::UnsupportedNode(node.kind))?;
                let pixels = input
                    .pixels
                    .iter()
                    .copied()
                    .map(|pixel| apply_operation(node_id, pixel, operation))
                    .collect::<Result<Vec<_>, _>>()?;
                CpuFrame::new(input.width, input.height, pixels)?
            }
            NodeKind::Output => evaluated_frame(&values, node_id, node.inputs[0])?.clone(),
            unsupported => return Err(CpuRenderError::UnsupportedNode(unsupported)),
        };
        values.insert(node_id, frame);
    }
    values
        .remove(&output)
        .ok_or(CpuRenderError::MissingSource(output))
}

fn evaluated_frame(
    values: &BTreeMap<NodeId, CpuFrame>,
    node: NodeId,
    input: NodeId,
) -> Result<&CpuFrame, CpuRenderError> {
    values
        .get(&input)
        .ok_or(CpuRenderError::MissingEvaluatedInput { node, input })
}

fn apply_operation(
    node: NodeId,
    input: Rgba,
    operation: CpuPixelOperation,
) -> Result<Rgba, CpuRenderError> {
    match operation {
        CpuPixelOperation::ColorGain { red, green, blue }
            if red.is_finite()
                && green.is_finite()
                && blue.is_finite()
                && red >= 0.0
                && green >= 0.0
                && blue >= 0.0
                && red <= 4.0
                && green <= 4.0
                && blue <= 4.0 =>
        {
            Rgba::new(
                (input.red * red).clamp(0.0, 1.0),
                (input.green * green).clamp(0.0, 1.0),
                (input.blue * blue).clamp(0.0, 1.0),
                input.alpha,
            )
            .map_err(CpuRenderError::Compositor)
        }
        CpuPixelOperation::Opacity(opacity)
            if opacity.is_finite() && (0.0..=1.0).contains(&opacity) =>
        {
            Rgba::new(input.red, input.green, input.blue, input.alpha * opacity)
                .map_err(CpuRenderError::Compositor)
        }
        _ => Err(CpuRenderError::InvalidOperation(node)),
    }
}

fn evaluated_input(
    values: &BTreeMap<NodeId, Rgba>,
    node: NodeId,
    input: NodeId,
) -> Result<Rgba, CpuRenderError> {
    values
        .get(&input)
        .copied()
        .ok_or(CpuRenderError::MissingEvaluatedInput { node, input })
}

impl RenderGraph {
    /// Creates an empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node and returns its stable identifier.
    ///
    /// # Errors
    ///
    /// Returns [`RenderGraphError::MissingInput`] or
    /// [`RenderGraphError::DuplicateInput`] when dependencies are invalid.
    pub fn add_node(
        &mut self,
        kind: NodeKind,
        inputs: &[NodeId],
    ) -> Result<NodeId, RenderGraphError> {
        let id = NodeId(self.next_id);
        if !valid_arity(kind, inputs.len()) {
            return Err(RenderGraphError::InvalidInputArity {
                node: id,
                kind,
                actual: inputs.len(),
            });
        }
        for (index, input) in inputs.iter().copied().enumerate() {
            if !self.nodes.contains_key(&input) {
                return Err(RenderGraphError::MissingInput { node: id, input });
            }
            if inputs[..index].contains(&input) {
                return Err(RenderGraphError::DuplicateInput { node: id, input });
            }
        }
        self.nodes.insert(
            id,
            RenderNode {
                id,
                kind,
                inputs: inputs.to_vec(),
            },
        );
        self.next_id = self.next_id.saturating_add(1);
        Ok(id)
    }

    /// Returns typed metadata for a node, if it exists.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&RenderNode> {
        self.nodes.get(&id)
    }

    /// Returns a stable topological evaluation order.
    ///
    /// Dependencies always precede consumers. A graph with independent nodes
    /// is ordered by node identifier.
    ///
    /// # Errors
    ///
    /// Returns [`RenderGraphError::Cycle`] when no valid evaluation order
    /// exists.
    pub fn evaluation_order(&self) -> Result<Vec<NodeId>, RenderGraphError> {
        let mut indegree = self
            .nodes
            .iter()
            .map(|(node, render_node)| (*node, render_node.inputs.len()))
            .collect::<BTreeMap<_, _>>();
        let mut consumers = BTreeMap::<NodeId, Vec<NodeId>>::new();
        for (node, render_node) in &self.nodes {
            for input in &render_node.inputs {
                consumers.entry(*input).or_default().push(*node);
            }
        }
        let mut ready = self
            .nodes
            .keys()
            .filter(|node| indegree[node] == 0)
            .copied()
            .collect::<BTreeSet<_>>();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(node) = ready.pop_first() {
            order.push(node);
            if let Some(dependents) = consumers.get(&node) {
                for dependent in dependents {
                    let Some(degree) = indegree.get_mut(dependent) else {
                        return Err(RenderGraphError::Cycle);
                    };
                    *degree -= 1;
                    if *degree == 0 {
                        ready.insert(*dependent);
                    }
                }
            }
        }
        if order.len() != self.nodes.len() {
            return Err(RenderGraphError::Cycle);
        }
        Ok(order)
    }

    /// Returns the number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns whether the graph has no nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

fn valid_arity(kind: NodeKind, input_count: usize) -> bool {
    match kind {
        NodeKind::MediaSource => input_count == 0,
        NodeKind::Transform
        | NodeKind::Crop
        | NodeKind::Color
        | NodeKind::Effect
        | NodeKind::Output => input_count == 1,
        NodeKind::Blend | NodeKind::Transition => input_count == 2,
        NodeKind::Text => input_count <= 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CpuPixelOperation, CpuRenderError, NodeKind, RenderGraph, RenderGraphError,
        evaluate_cpu_pixel, evaluate_cpu_pixel_with_operations,
    };
    use prism_compositor::{BlendMode, Rgba};
    use std::collections::BTreeMap;

    #[test]
    fn produces_dependency_first_deterministic_order() -> Result<(), RenderGraphError> {
        let mut graph = RenderGraph::new();
        let source = graph.add_node(NodeKind::MediaSource, &[])?;
        let overlay = graph.add_node(NodeKind::MediaSource, &[])?;
        let composite = graph.add_node(NodeKind::Blend, &[source, overlay])?;
        let output = graph.add_node(NodeKind::Output, &[composite])?;
        assert_eq!(
            graph.evaluation_order()?,
            vec![source, overlay, composite, output]
        );
        assert_eq!(graph.len(), 4);
        assert!(!graph.is_empty());
        assert_eq!(
            graph.node(composite).map(|node| node.kind),
            Some(NodeKind::Blend)
        );
        Ok(())
    }

    #[test]
    fn rejects_unknown_and_duplicate_dependencies() -> Result<(), RenderGraphError> {
        let mut graph = RenderGraph::new();
        let missing = super::NodeId(42);
        assert!(matches!(
            graph.add_node(NodeKind::Effect, &[missing]),
            Err(RenderGraphError::MissingInput { .. })
        ));
        let source = graph.add_node(NodeKind::MediaSource, &[])?;
        assert!(matches!(
            graph.add_node(NodeKind::Blend, &[source, source]),
            Err(RenderGraphError::DuplicateInput { .. })
        ));
        assert!(matches!(
            graph.add_node(NodeKind::Output, &[]),
            Err(RenderGraphError::InvalidInputArity { .. })
        ));
        Ok(())
    }

    #[test]
    fn cycle_detection_is_explicit() -> Result<(), RenderGraphError> {
        let mut graph = RenderGraph::new();
        let first = graph.add_node(NodeKind::MediaSource, &[])?;
        let second = graph.add_node(NodeKind::Transform, &[first])?;
        if let Some(node) = graph.nodes.get_mut(&first) {
            node.inputs.push(second);
        }
        assert_eq!(graph.evaluation_order(), Err(RenderGraphError::Cycle));
        Ok(())
    }

    #[test]
    fn cpu_renderer_evaluates_transition_and_output() -> Result<(), Box<dyn std::error::Error>> {
        let mut graph = RenderGraph::new();
        let outgoing = graph.add_node(NodeKind::MediaSource, &[])?;
        let incoming = graph.add_node(NodeKind::MediaSource, &[])?;
        let transition = graph.add_node(NodeKind::Transition, &[outgoing, incoming])?;
        let output = graph.add_node(NodeKind::Output, &[transition])?;
        let mut sources = BTreeMap::new();
        sources.insert(outgoing, Rgba::new(0.0, 0.0, 0.0, 1.0)?);
        sources.insert(incoming, Rgba::new(1.0, 1.0, 1.0, 1.0)?);
        assert_eq!(
            evaluate_cpu_pixel(&graph, output, &sources, 0.5)?,
            Rgba::new(0.5, 0.5, 0.5, 1.0)?
        );
        Ok(())
    }

    #[test]
    fn cpu_renderer_evaluates_normal_blend() -> Result<(), Box<dyn std::error::Error>> {
        let mut graph = RenderGraph::new();
        let background = graph.add_node(NodeKind::MediaSource, &[])?;
        let foreground = graph.add_node(NodeKind::MediaSource, &[])?;
        let blend = graph.add_node(NodeKind::Blend, &[background, foreground])?;
        let output = graph.add_node(NodeKind::Output, &[blend])?;
        let mut sources = BTreeMap::new();
        sources.insert(background, Rgba::new(0.0, 0.0, 0.0, 1.0)?);
        sources.insert(foreground, Rgba::new(1.0, 0.0, 0.0, 0.5)?);
        assert_eq!(
            evaluate_cpu_pixel(&graph, output, &sources, 0.0)?,
            Rgba::new(0.5, 0.0, 0.0, 1.0)?
        );
        Ok(())
    }

    #[test]
    fn cpu_renderer_applies_parameterized_color_and_opacity()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut graph = RenderGraph::new();
        let source = graph.add_node(NodeKind::MediaSource, &[])?;
        let color = graph.add_node(NodeKind::Color, &[source])?;
        let transform = graph.add_node(NodeKind::Transform, &[color])?;
        let output = graph.add_node(NodeKind::Output, &[transform])?;
        let mut sources = BTreeMap::new();
        sources.insert(source, Rgba::new(0.4, 0.5, 0.6, 1.0)?);
        let mut operations = BTreeMap::new();
        operations.insert(
            color,
            CpuPixelOperation::ColorGain {
                red: 2.0,
                green: 1.0,
                blue: 0.5,
            },
        );
        operations.insert(transform, CpuPixelOperation::Opacity(0.25));
        assert_eq!(
            evaluate_cpu_pixel_with_operations(&graph, output, &sources, 0.0, &operations)?,
            Rgba::new(0.8, 0.5, 0.3, 0.25)?
        );
        Ok(())
    }

    #[test]
    fn cpu_renderer_applies_configured_blend_mode() -> Result<(), Box<dyn std::error::Error>> {
        let mut graph = RenderGraph::new();
        let background = graph.add_node(NodeKind::MediaSource, &[])?;
        let foreground = graph.add_node(NodeKind::MediaSource, &[])?;
        let blend = graph.add_node(NodeKind::Blend, &[background, foreground])?;
        let output = graph.add_node(NodeKind::Output, &[blend])?;
        let mut sources = BTreeMap::new();
        sources.insert(background, Rgba::new(0.2, 0.4, 0.6, 1.0)?);
        sources.insert(foreground, Rgba::new(0.8, 0.5, 0.25, 1.0)?);
        let mut operations = BTreeMap::new();
        operations.insert(blend, CpuPixelOperation::Blend(BlendMode::Multiply));
        let result =
            evaluate_cpu_pixel_with_operations(&graph, output, &sources, 0.0, &operations)?;
        let expected = Rgba::new(0.16, 0.2, 0.15, 1.0)?;
        assert!((result.red - expected.red).abs() < 1e-6);
        assert!((result.green - expected.green).abs() < 1e-6);
        assert!((result.blue - expected.blue).abs() < 1e-6);
        assert!((result.alpha - expected.alpha).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn cpu_renderer_rejects_missing_sources_and_unsupported_nodes()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut graph = RenderGraph::new();
        let source = graph.add_node(NodeKind::MediaSource, &[])?;
        let output = graph.add_node(NodeKind::Output, &[source])?;
        assert_eq!(
            evaluate_cpu_pixel(&graph, output, &BTreeMap::new(), 0.0),
            Err(CpuRenderError::MissingSource(source))
        );
        let mut unsupported = RenderGraph::new();
        let source = unsupported.add_node(NodeKind::MediaSource, &[])?;
        let color = unsupported.add_node(NodeKind::Color, &[source])?;
        let output = unsupported.add_node(NodeKind::Output, &[color])?;
        let mut sources = BTreeMap::new();
        sources.insert(source, Rgba::new(0.1, 0.2, 0.3, 1.0)?);
        assert_eq!(
            evaluate_cpu_pixel(&unsupported, output, &sources, 0.0),
            Err(CpuRenderError::UnsupportedNode(NodeKind::Color))
        );
        Ok(())
    }
}
