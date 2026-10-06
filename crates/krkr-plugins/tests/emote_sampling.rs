// Exercise the production decoder/model/sampler without a window or GPU.
#[path = "../src/psb/decode.rs"]
#[allow(dead_code)]
pub(crate) mod decode;
mod psb {
    pub(crate) use super::decode;
}
#[path = "../src/emote/model.rs"]
#[allow(dead_code)]
mod model;
#[path = "../src/emote/sample.rs"]
#[allow(dead_code)]
mod sample;
use decode::Node;

fn motion(times: &[f64]) -> model::Motion {
    let frames = times
        .iter()
        .enumerate()
        .map(|(i, &time)| {
            Node::Object(vec![
                ("time".into(), Node::Real(time)),
                (
                    "content".into(),
                    Node::Object(vec![("opa".into(), Node::Real(i as f64))]),
                ),
            ])
        })
        .collect();
    model::Motion::read(
        &Node::Object(vec![(
            "layer".into(),
            Node::Array(vec![Node::Object(vec![(
                "frameList".into(),
                Node::Array(frames),
            )])]),
        )]),
        model::Format {
            krkr: true,
            motion: false,
        },
        &|| false,
    )
    .unwrap()
}

#[test]
fn frame_selection_handles_boundaries_duplicate_times_and_legacy_unordered_data() {
    let mut ordered: Vec<_> = (0..2048).map(|i| (i / 2) as f64).collect();
    for times in [
        ordered.clone(),
        vec![],
        vec![f64::NAN],
        vec![0., 10., 5., 20.],
    ] {
        let motion = motion(&times);
        let layer = &motion.layers[0];
        for tick in [
            -1.,
            0.,
            0.5,
            1.,
            8.,
            10.,
            1023.,
            2048.,
            f32::INFINITY,
            f32::NAN,
        ] {
            let expected = times
                .iter()
                .take_while(|&&time| time <= f64::from(tick))
                .count()
                .checked_sub(1);
            let sampled = layer.sample(
                tick,
                &motion,
                false,
                None,
                sample::Bounds {
                    size: [32.; 2],
                    origin: [0.; 2],
                },
            );
            assert_eq!(sampled.map(|s| s.frame), expected);
        }
    }
    ordered.push(f64::INFINITY);
    assert!(!motion(&ordered).layers[0].ordered_frames);
    let motion = motion(&[0., 10.]);
    let sample = motion.layers[0]
        .sample(
            5.,
            &motion,
            false,
            None,
            sample::Bounds {
                size: [32.; 2],
                origin: [0.; 2],
            },
        )
        .unwrap();
    assert_eq!(sample.opacity, 0.5);
}
