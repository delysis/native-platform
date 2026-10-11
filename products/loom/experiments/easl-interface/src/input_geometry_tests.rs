use super::*;

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

#[test]
fn pointer_and_pixel_scroll_use_the_same_physical_to_logical_scale() {
    for scale in [1., 1.25, 1.5, 2., 3.] {
        let point = logical_point([150. * scale, 72. * scale], scale).unwrap();
        close(point[0], 150.);
        close(point[1], 72.);
        close(pixel_scroll_steps(48. * scale, scale).unwrap(), -2.);
        close(pixel_scroll_steps(-24. * scale, scale).unwrap(), 1.);
    }
}

#[test]
fn fractional_scroll_is_not_rounded_to_a_line() {
    close(pixel_scroll_steps(1., 2.).unwrap(), -1. / 48.);
    close(pixel_scroll_steps(0., 2.).unwrap(), 0.);
}

#[test]
fn off_window_pointer_coordinates_remain_available_to_captured_drags() {
    let point = logical_point([-40., 100.], 2.).unwrap();
    close(point[0], -20.);
    close(point[1], 50.);
}

#[test]
fn malformed_scale_coordinates_and_unrepresentable_results_are_rejected() {
    for scale in [
        0.,
        -1.,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::from_bits(1),
    ] {
        assert!(logical_point([1., 2.], scale).is_none());
        assert!(pixel_scroll_steps(1., scale).is_none());
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        assert!(logical_point([value, 1.], 1.).is_none());
        assert!(logical_point([1., value], 1.).is_none());
        assert!(pixel_scroll_steps(value, 1.).is_none());
    }
    assert!(logical_point([1., 1.], f64::MIN_POSITIVE).is_none());
}
