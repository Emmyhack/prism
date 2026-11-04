# ADR-003: GPU rendering

Status: Provisional. Use wgpu for Metal, Vulkan, and Direct3D 12 portability,
WGSL validation, resource accounting, and device-loss recovery. Keep the render
graph independent of wgpu handles. Permit narrow native Metal paths only after a
profiled limitation and equivalent CPU/export semantics are documented. Gate on
external video texture import, HDR formats, shader cache, and sustained 4K tests.
