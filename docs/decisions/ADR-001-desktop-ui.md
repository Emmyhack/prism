# ADR-001: Desktop UI

Status: Provisional. Select Slint behind a thin Studio adapter. It is Rust-native,
cross-platform, GPU-capable, and royalty-free for desktop distribution with
required attribution. Qt/QML is more mature for docking, accessibility, and rich
text editing, but adds C++/binding complexity and LGPL/commercial deployment
obligations. Slint must pass prototypes for virtualized timelines, docking,
accessibility, IME, high DPI, multi-window, and wgpu texture interop. Failure of
those gates reopens Qt/QML; no domain crate may depend on either toolkit.
