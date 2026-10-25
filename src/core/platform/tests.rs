use super::{CapturePolicy, DisplayBounds, PeerEdge, Point, TransferMode};

#[test]
fn point_and_display_bounds_are_value_types() {
    let point = Point { x: 42, y: -7 };
    assert_eq!(point, Point { x: 42, y: -7 });

    let bounds = DisplayBounds {
        x: -100,
        y: 200,
        width: 1920,
        height: 1080,
    };
    assert_eq!(
        bounds,
        DisplayBounds {
            x: -100,
            y: 200,
            width: 1920,
            height: 1080,
        }
    );
}

#[test]
fn transfer_mode_discriminants_are_stable() {
    assert_eq!(TransferMode::LocalControl as u8, 0);
    assert_eq!(TransferMode::RemoteControl as u8, 1);
}

#[test]
fn capture_policy_is_derived_from_ownership() {
    assert_eq!(
        CapturePolicy::from_transfer_mode(TransferMode::LocalControl),
        CapturePolicy::DeliverLocally
    );
    assert_eq!(
        CapturePolicy::from_transfer_mode(TransferMode::RemoteControl),
        CapturePolicy::SuppressLocalDelivery
    );
    assert_eq!(CapturePolicy::from_u8(255), CapturePolicy::DeliverLocally);
}

#[test]
fn peer_edge_as_str_matches_serialized_labels() {
    assert_eq!(PeerEdge::Left.as_str(), "left");
    assert_eq!(PeerEdge::Right.as_str(), "right");
    assert_eq!(PeerEdge::Top.as_str(), "top");
    assert_eq!(PeerEdge::Bottom.as_str(), "bottom");
}

#[test]
fn default_trait_implementations_fail_safely() {
    struct MinimalInjection;
    impl super::traits::EventInjection for MinimalInjection {
        fn inject_mouse_move(&self, _x: i32, _y: i32) -> anyhow::Result<()> {
            Ok(())
        }
        fn inject_mouse_button(&self, _button: u8, _pressed: bool) -> anyhow::Result<()> {
            Ok(())
        }
        fn inject_mouse_wheel(&self, _delta_x: i32, _delta_y: i32) -> anyhow::Result<()> {
            Ok(())
        }
        fn set_cursor_hidden(&self, _hidden: bool) -> anyhow::Result<()> {
            Ok(())
        }
        fn warp_cursor(&self, _x: i32, _y: i32) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let injection = MinimalInjection;
    assert!(super::traits::EventInjection::inject_mouse_move_relative(&injection, 10, -5).is_err());
    assert!(super::traits::EventInjection::declare_user_activity(&injection).is_ok());

    struct MinimalDisplay;
    impl super::traits::DisplayManager for MinimalDisplay {
        fn discover_displays(&self) -> anyhow::Result<Vec<DisplayBounds>> {
            Ok(Vec::new())
        }
        fn get_virtual_bounds(&self) -> anyhow::Result<DisplayBounds> {
            Ok(DisplayBounds { x: 0, y: 0, width: 1, height: 1 })
        }
        fn contains_point(&self, _x: i32, _y: i32) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn clamp_to_active_display(&self, x: i32, y: i32) -> anyhow::Result<(i32, i32)> {
            Ok((x, y))
        }
        fn get_signature(&self) -> anyhow::Result<u64> {
            Ok(0)
        }
    }

    let display = MinimalDisplay;
    assert!(super::traits::DisplayManager::cursor_position(&display).is_err());
}
