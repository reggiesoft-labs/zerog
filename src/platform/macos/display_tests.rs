use super::{
    is_within_edge_buffer, map_remote_mouse_to_local, parse_remote_display, DisplayTopology,
    RemoteDisplay, ScreenBounds,
};

fn topology(displays: Vec<ScreenBounds>) -> DisplayTopology {
    let virtual_bounds = super::combined_bounds(&displays);
    DisplayTopology {
        displays,
        virtual_bounds,
        signature: 1,
        mirrored_display_count: 0,
    }
}

#[test]
fn parse_remote_display_requires_valid_dimensions() {
    let parsed = parse_remote_display("1920x1080").expect("valid remote display");
    assert_eq!(parsed.width, 1920.0);
    assert_eq!(parsed.height, 1080.0);

    assert!(parse_remote_display("0x1080").is_none());
    assert!(parse_remote_display("100x1").is_none());
    assert!(parse_remote_display("invalid").is_none());
}

#[test]
fn map_remote_to_local_scales_between_resolutions() {
    let local = topology(vec![ScreenBounds {
        min_x: 0.0,
        max_x: 1279.0,
        min_y: 0.0,
        max_y: 719.0,
    }]);
    let remote = Some(RemoteDisplay {
        width: 1920.0,
        height: 1080.0,
    });

    let origin = map_remote_mouse_to_local(0, 0, &local, remote);
    assert_eq!(origin.x, 0.0);
    assert_eq!(origin.y, 0.0);

    let far_edge = map_remote_mouse_to_local(1919, 1079, &local, remote);
    assert_eq!(far_edge.x, 1279.0);
    assert_eq!(far_edge.y, 719.0);

    let center = map_remote_mouse_to_local(960, 540, &local, remote);
    assert!(center.x > 638.0 && center.x < 642.0);
    assert!(center.y > 358.0 && center.y < 362.0);
}

#[test]
fn map_remote_to_local_clamps_out_of_bounds_coordinates() {
    let local = topology(vec![ScreenBounds {
        min_x: 10.0,
        max_x: 110.0,
        min_y: 20.0,
        max_y: 220.0,
    }]);
    let remote = Some(RemoteDisplay {
        width: 100.0,
        height: 50.0,
    });

    let low = map_remote_mouse_to_local(-1000, -1000, &local, remote);
    assert_eq!(low.x, 10.0);
    assert_eq!(low.y, 20.0);

    let high = map_remote_mouse_to_local(1000, 1000, &local, remote);
    assert_eq!(high.x, 110.0);
    assert_eq!(high.y, 220.0);
}

#[test]
fn map_remote_to_local_avoids_gap_between_displays() {
    let local = topology(vec![
        ScreenBounds {
            min_x: 0.0,
            max_x: 99.0,
            min_y: 0.0,
            max_y: 99.0,
        },
        ScreenBounds {
            min_x: 200.0,
            max_x: 299.0,
            min_y: 0.0,
            max_y: 99.0,
        },
    ]);
    let remote = Some(RemoteDisplay {
        width: 300.0,
        height: 100.0,
    });

    let point = map_remote_mouse_to_local(150, 50, &local, remote);
    assert_eq!(point.y, 50.0);
    assert!(point.x == 99.0 || point.x == 200.0);
}

#[test]
fn edge_detection_uses_virtual_boundary_segments() {
    let local = topology(vec![
        ScreenBounds {
            min_x: 0.0,
            max_x: 1919.0,
            min_y: 0.0,
            max_y: 1079.0,
        },
        ScreenBounds {
            min_x: 1920.0,
            max_x: 3839.0,
            min_y: 200.0,
            max_y: 1279.0,
        },
    ]);

    assert!(is_within_edge_buffer(
        3839.0,
        150.0,
        super::PeerEdge::Right,
        &local
    ));
    assert!(!is_within_edge_buffer(
        1919.0,
        300.0,
        super::PeerEdge::Right,
        &local
    ));
}
