# Prism editing icons

Original 24×24 monochrome SVG assets for Prism, using familiar nonlinear-editor
conventions. These are not extracted Adobe assets. Visual references: supplied
Premiere Pro screenshots and Adobe's Tools panel documentation:
https://helpx.adobe.com/premiere/desktop/get-started/tour-the-workspace/tools-panel-and-options-panel.html

The editing rail uses a selection pointer, diagonal razor, edge trim, slip, slide
and rolling-edit symbols. Transport uses play/stop, frame-step and edit-boundary
symbols. Other controls use source In/Out braces, marker, snapping magnet, lock,
visibility eye, safe-margin guides, zoom, fit, undo/redo, Home and search symbols.
Audio track mute retains its familiar M label.

The shared Action component renders icons at 18px and preserves enabled/disabled,
active, hover, focus, keyboard and accessible-label behavior. Prism's violet active
state remains consistent with the rest of the workspace. Tooltips/context help name
the operation and current Prism shortcut. Existing command bindings are preserved;
this change does not claim Premiere keyboard-map parity or add unsupported tools.

File and clip actions also use folder-open, new-project, save/save-as, import/export,
duplicate, delete and link/unlink icons. Named actions use `with-icon` so their labels
remain visible; navigation may use a trailing arrow. Icon-only tools retain their
accessible names and hover help. The same SVG tinting handles disabled and active
states across both variants.
