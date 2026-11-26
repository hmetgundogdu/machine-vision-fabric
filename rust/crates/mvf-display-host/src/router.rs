//! Input authorization: a node receives only what its contract grants. Window lifecycle events
//! (focus, resize, close) are always delivered: they inform the node and trigger nothing.

use mvf_display::{Grants, InputKind};

/// Whether `kind` may reach a node with `grants`.
pub fn admits(kind: InputKind, grants: Grants) -> bool {
    kind.is_window()
        || (kind.is_keyboard() && grants.keyboard)
        || (kind.is_pointer() && grants.mouse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_granted_input_is_routed() {
        let none = Grants::default();
        let mouse = Grants {
            mouse: true,
            keyboard: false,
        };
        assert!(admits(InputKind::Resize, none) && admits(InputKind::CloseRequested, none));
        assert!(!admits(InputKind::PointerDown, none) && !admits(InputKind::KeyDown, none));
        assert!(admits(InputKind::Wheel, mouse) && !admits(InputKind::KeyUp, mouse));
    }
}
