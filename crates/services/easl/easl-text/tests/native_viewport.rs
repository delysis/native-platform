use easl_text::Viewport;
#[test]
fn shared_easl_viewport_reveals_and_clamps_both_axes_and_recovers_from_bad_input() {
    let mut view = Viewport::new().unwrap();
    assert_eq!(
        view.reveal(
            [0., 0.],
            [100., 30.],
            [1000., 200.],
            [500., 80., 2., 20.],
            [2., 2.]
        )
        .unwrap(),
        [404., 72.]
    );
    assert_eq!(
        view.reveal(
            [404., 72.],
            [100., 30.],
            [1000., 200.],
            [0., 0., 2., 20.],
            [2., 2.]
        )
        .unwrap(),
        [0., 0.]
    );
    assert!(
        view.reveal(
            [0., 0.],
            [0., 30.],
            [100., 100.],
            [0., 0., 1., 1.],
            [0., 0.]
        )
        .is_err()
    );
    assert!(
        view.reveal(
            [f32::NAN, 0.],
            [100., 30.],
            [100., 100.],
            [0., 0., 1., 1.],
            [0., 0.]
        )
        .is_err()
    );
    assert_eq!(
        view.reveal(
            [100., 100.],
            [100., 30.],
            [10., 10.],
            [0., 0., 1., 1.],
            [0., 0.]
        )
        .unwrap(),
        [0., 0.]
    );
}
