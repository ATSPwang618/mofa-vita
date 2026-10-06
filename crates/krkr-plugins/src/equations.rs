//! `equations.dll`/`.tpm`: Robert Penner easing equations exposed as the
//! `Equations` class. The reference keeps the algorithms in `calc` behind a
//! 42-entry table where indexes 0 and 1 both select `easeNone`; the named
//! methods are the same functions reachable directly.
krkr_engine::native_plugin! {
    pub(crate) Equations {
        names: ["equations.dll", "equations.tpm"],
        classes: [bindings],
        extensions: [],
    }
}

/// Dispatch order of the reference table; `calc` clamps anything outside it to
/// index 0.
type Equation = fn(f64, f64, f64, f64) -> f64;
const FUNCTIONS: [Equation; 42] = [
    ease_none,
    ease_none,
    ease_in_quad,
    ease_out_quad,
    ease_in_out_quad,
    ease_out_in_quad,
    ease_in_cubic,
    ease_out_cubic,
    ease_in_out_cubic,
    ease_out_in_cubic,
    ease_in_quart,
    ease_out_quart,
    ease_in_out_quart,
    ease_out_in_quart,
    ease_in_quint,
    ease_out_quint,
    ease_in_out_quint,
    ease_out_in_quint,
    ease_in_sine,
    ease_out_sine,
    ease_in_out_sine,
    ease_out_in_sine,
    ease_in_circ,
    ease_out_circ,
    ease_in_out_circ,
    ease_out_in_circ,
    ease_in_expo,
    ease_out_expo,
    ease_in_out_expo,
    ease_out_in_expo,
    ease_in_elastic,
    ease_out_elastic,
    ease_in_out_elastic,
    ease_out_in_elastic,
    ease_in_back,
    ease_out_back,
    ease_in_out_back,
    ease_out_in_back,
    ease_in_bounce,
    ease_out_bounce,
    ease_in_out_bounce,
    ease_out_in_bounce,
];

#[tjs_bind::class(name = "Equations")]
mod bindings {
    use super::*;
    use tjs_core::{NativeCx, NativeResult, Value, value};
    #[derive(Default, tjs_bind::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method]
        fn calc(
            cx: &mut NativeCx<'_>,
            n: Value,
            a1: f64,
            a2: f64,
            a3: f64,
            a4: f64,
        ) -> NativeResult<f64> {
            // Keyframe acceleration fields arrive as strings from stringUtil.
            let n = value::to_integer(cx.heap(), n)? as i32;
            let index = if n < 0 || n as usize >= FUNCTIONS.len() {
                0
            } else {
                n as usize
            };
            Ok(FUNCTIONS[index](a1, a2, a3, a4))
        }
        #[tjs::method(name = "easeNone")]
        fn ease_none(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_none(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInQuad")]
        fn ease_in_quad(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_quad(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutQuad")]
        fn ease_out_quad(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_quad(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutQuad")]
        fn ease_in_out_quad(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_quad(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInQuad")]
        fn ease_out_in_quad(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_quad(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInCubic")]
        fn ease_in_cubic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_cubic(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutCubic")]
        fn ease_out_cubic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_cubic(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutCubic")]
        fn ease_in_out_cubic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_cubic(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInCubic")]
        fn ease_out_in_cubic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_cubic(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInQuart")]
        fn ease_in_quart(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_quart(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutQuart")]
        fn ease_out_quart(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_quart(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutQuart")]
        fn ease_in_out_quart(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_quart(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInQuart")]
        fn ease_out_in_quart(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_quart(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInQuint")]
        fn ease_in_quint(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_quint(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutQuint")]
        fn ease_out_quint(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_quint(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutQuint")]
        fn ease_in_out_quint(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_quint(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInQuint")]
        fn ease_out_in_quint(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_quint(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInSine")]
        fn ease_in_sine(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_sine(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutSine")]
        fn ease_out_sine(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_sine(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutSine")]
        fn ease_in_out_sine(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_sine(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInSine")]
        fn ease_out_in_sine(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_sine(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInCirc")]
        fn ease_in_circ(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_circ(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutCirc")]
        fn ease_out_circ(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_circ(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutCirc")]
        fn ease_in_out_circ(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_circ(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInCirc")]
        fn ease_out_in_circ(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_circ(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInExpo")]
        fn ease_in_expo(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_expo(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutExpo")]
        fn ease_out_expo(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_expo(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutExpo")]
        fn ease_in_out_expo(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_expo(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInExpo")]
        fn ease_out_in_expo(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_expo(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInElastic")]
        fn ease_in_elastic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_elastic(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutElastic")]
        fn ease_out_elastic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_elastic(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutElastic")]
        fn ease_in_out_elastic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_elastic(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInElastic")]
        fn ease_out_in_elastic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_elastic(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInBack")]
        fn ease_in_back(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_back(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutBack")]
        fn ease_out_back(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_back(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutBack")]
        fn ease_in_out_back(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_back(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInBack")]
        fn ease_out_in_back(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_back(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInBounce")]
        fn ease_in_bounce(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_bounce(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutBounce")]
        fn ease_out_bounce(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_bounce(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeInOutBounce")]
        fn ease_in_out_bounce(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_in_out_bounce(a1, a2, a3, a4)
        }
        #[tjs::method(name = "easeOutInBounce")]
        fn ease_out_in_bounce(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
            super::ease_out_in_bounce(a1, a2, a3, a4)
        }
    }
}

// a1 = elapsed, a2 = begin, a3 = change, a4 = duration. The reference uses
// x87 floating-point operations. Keep operand order, but do not promise bitwise
// equality between x87 intermediate precision and portable f64/libm results.
fn ease_none(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    a1 * a3 / a4 + a2
}
fn ease_in_quad(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4;
    a3 * t * t + a2
}
fn ease_out_quad(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4;
    a2 - a3 * t * (t - 2.0)
}
fn ease_in_out_quad(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / (a4 * 0.5);
    if t >= 1.0 {
        a2 - ((t - 1.0) * (t - 3.0) - 1.0) * 0.5 * a3
    } else {
        t * t * 0.5 * a3 + a2
    }
}
fn ease_out_in_quad(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 >= a4 * 0.5 {
        let t = (a1 + a1 - a4) / a4;
        t * 0.5 * a3 * t + 0.5 * a3 + a2
    } else {
        a2 - 0.5 * a3 * a1 * 2.0 / a4 * (a1 * 2.0 / a4 - 2.0)
    }
}
fn ease_in_cubic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4;
    t * t * t * a3 + a2
}
fn ease_out_cubic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4 - 1.0;
    (t * t * t + 1.0) * a3 + a2
}
fn ease_in_out_cubic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / (a4 * 0.5);
    if t >= 1.0 {
        let u = t - 2.0;
        (u * u * u + 2.0) * 0.5 * a3 + a2
    } else {
        t * t * t * 0.5 * a3 + a2
    }
}
fn ease_out_in_cubic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let d = a1 + a1;
    if a1 >= a4 * 0.5 {
        let t = (d - a4) / a4;
        t * 0.5 * a3 * t * t + 0.5 * a3 + a2
    } else {
        let t = d / a4 - 1.0;
        (t * t * t + 1.0) * 0.5 * a3 + a2
    }
}
fn ease_in_quart(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4;
    t * t * t * t * a3 + a2
}
fn ease_out_quart(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4 - 1.0;
    (1.0 - t * t * t * t) * a3 + a2
}
fn ease_in_out_quart(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / (a4 * 0.5);
    if t >= 1.0 {
        let u = t - 2.0;
        a2 - (u * u * u * u - 2.0) * 0.5 * a3
    } else {
        t * t * t * t * 0.5 * a3 + a2
    }
}
fn ease_out_in_quart(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let d = a1 + a1;
    if a1 >= a4 * 0.5 {
        let t = (d - a4) / a4;
        t * 0.5 * a3 * t * t * t + 0.5 * a3 + a2
    } else {
        let t = d / a4 - 1.0;
        (1.0 - t * t * t * t) * 0.5 * a3 + a2
    }
}
fn ease_in_quint(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4;
    t * t * t * t * t * a3 + a2
}
fn ease_out_quint(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4 - 1.0;
    (t * t * t * t * t + 1.0) * a3 + a2
}
fn ease_in_out_quint(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / (a4 * 0.5);
    if t >= 1.0 {
        let u = t - 2.0;
        (u * u * u * u * u + 2.0) * 0.5 * a3 + a2
    } else {
        t * t * t * t * t * 0.5 * a3 + a2
    }
}
fn ease_out_in_quint(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let d = a1 + a1;
    if a1 >= a4 * 0.5 {
        let t = (d - a4) / a4;
        t * 0.5 * a3 * t * t * t * t + 0.5 * a3 + a2
    } else {
        let t = d / a4 - 1.0;
        (t * t * t * t * t + 1.0) * 0.5 * a3 + a2
    }
}
fn ease_in_sine(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    a3 - (a1 / a4 * std::f64::consts::FRAC_PI_2).cos() * a3 + a2
}
fn ease_out_sine(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    (a1 / a4 * std::f64::consts::FRAC_PI_2).sin() * a3 + a2
}
fn ease_in_out_sine(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    a2 - ((a1 * std::f64::consts::PI / a4).cos() - 1.0) * a3 * 0.5
}
fn ease_out_in_sine(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 >= a4 * 0.5 {
        0.5 * a3 - ((a1 + a1 - a4) / a4 * std::f64::consts::FRAC_PI_2).cos() * 0.5 * a3
            + 0.5 * a3
            + a2
    } else {
        ((a1 + a1) / a4 * std::f64::consts::FRAC_PI_2).sin() * a3 * 0.5 + a2
    }
}
fn ease_in_circ(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4;
    a2 - ((1.0 - t * t).sqrt() - 1.0) * a3
}
fn ease_out_circ(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4 - 1.0;
    (1.0 - t * t).sqrt() * a3 + a2
}
fn ease_in_out_circ(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / (a4 * 0.5);
    if t >= 1.0 {
        ((1.0 - (t - 2.0) * (t - 2.0)).sqrt() + 1.0) * a3 * 0.5 + a2
    } else {
        a2 - ((1.0 - t * t).sqrt() - 1.0) * a3 * 0.5
    }
}
fn ease_out_in_circ(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 >= a4 * 0.5 {
        let t = (a1 + a1 - a4) / a4;
        0.5 * a3 + a2 - ((1.0 - t * t).sqrt() - 1.0) * 0.5 * a3
    } else {
        let t = (a1 + a1) / a4 - 1.0;
        (1.0 - t * t).sqrt() * a3 * 0.5 + a2
    }
}
fn ease_in_expo(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 == 0.0 {
        a2
    } else {
        2f64.powf((a1 / a4 - 1.0) * 10.0) * a3 + a2 - a3 * 0.001
    }
}
fn ease_out_expo(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 == a4 {
        a2 + a3
    } else {
        (1.0 - 2f64.powf(a1 * -10.0 / a4)) * a3 * 1.001 + a2
    }
}
fn ease_in_out_expo(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 == 0.0 {
        a2
    } else if a4 == a1 {
        a2 + a3
    } else {
        let t = a1 / (a4 * 0.5);
        let u = t - 1.0;
        if 1.0 <= t {
            (2.0 - 2f64.powf(u * -10.0)) * a3 * 0.5 * 1.0005 + a2
        } else {
            2f64.powf(u * 10.0) * 0.5 * a3 + a2 - a3 * 0.0005
        }
    }
}
fn ease_out_in_expo(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let d = a1 * 2.0;
    if a1 >= a4 * 0.5 {
        if d - a4 == 0.0 {
            0.5 * a3 + a2
        } else {
            2f64.powf(((d - a4) / a4 - 1.0) * 10.0) * 0.5 * a3 + 0.5 * a3 + a2 - 0.5 * a3 * 0.001
        }
    } else {
        let half = 0.5 * a3;
        if a4 == d {
            half + a2
        } else {
            (1.0 - 2f64.powf(d * -10.0 / a4)) * half * 1.001 + a2
        }
    }
}
fn ease_in_elastic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 == 0.0 {
        return a2;
    }
    let t = a1 / a4;
    if 1.0 == t {
        return a2 + a3;
    }
    let s = a4 * 0.3;
    a2 - 2f64.powf((t - 1.0) * 10.0)
        * a3
        * ((a4 * (t - 1.0) - s * 0.25) * std::f64::consts::TAU / s).sin()
}
fn ease_out_elastic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 == 0.0 {
        return a2;
    }
    let t = a1 / a4;
    if 1.0 == t {
        return a2 + a3;
    }
    let s = a4 * 0.3;
    2f64.powf(t * -10.0) * a3 * ((a4 * t - s * 0.25) * std::f64::consts::TAU / s).sin() + a3 + a2
}
fn ease_in_out_elastic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 == 0.0 {
        return a2;
    }
    let t = a1 / (0.5 * a4);
    if 2.0 == t {
        return a2 + a3;
    }
    let s = a4 * 0.45;
    let u = t - 1.0;
    let angle = std::f64::consts::TAU * (a4 * u - 0.25 * s) / s;
    if t >= 1.0 {
        2f64.powf(u * -10.0) * a3 * angle.sin() * 0.5 + a3 + a2
    } else {
        a2 - 2f64.powf(u * 10.0) * a3 * angle.sin() * 0.5
    }
}
fn ease_out_in_elastic(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 >= a4 * 0.5 {
        ease_in_elastic(a1 + a1 - a4, 0.5 * a3 + a2, 0.5 * a3, a4)
    } else {
        ease_out_elastic(a1 + a1, a2, 0.5 * a3, a4)
    }
}
fn ease_in_back(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4;
    a3 * t * t * (t * 2.70158 - 1.70158) + a2
}
fn ease_out_back(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4 - 1.0;
    (t * t * (t * 2.70158 + 1.70158) + 1.0) * a3 + a2
}
fn ease_in_out_back(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    const S: f64 = 1.70158;
    let s = S * 1.525;
    let t = a1 / (a4 * 0.5);
    if t >= 1.0 {
        let u = t - 2.0;
        (u * u * (s + (s + 1.0) * u) + 2.0) * 0.5 * a3 + a2
    } else {
        (t * (s + 1.0) - s) * t * t * 0.5 * a3 + a2
    }
}
fn ease_out_in_back(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    const S: f64 = 1.70158;
    let d = a1 + a1;
    if a1 >= a4 * 0.5 {
        let t = (d - a4) / a4;
        t * 0.5 * a3 * t * ((S + 1.0) * t - S) + 0.5 * a3 + a2
    } else {
        let t = d / a4 - 1.0;
        (t * t * (S + (S + 1.0) * t) + 1.0) * 0.5 * a3 + a2
    }
}
fn ease_out_bounce(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let t = a1 / a4;
    if t >= 0.3636363636363636 {
        let (offset, scale) = if t >= 0.7272727272727273 {
            if t >= 0.9090909090909091 {
                (0.9545454545454546, 0.984375)
            } else {
                (0.8181818181818182, 0.9375)
            }
        } else {
            (0.5454545454545454, 0.75)
        };
        ((t - offset) * 7.5625 * (t - offset) + scale) * a3 + a2
    } else {
        t * t * 7.5625 * a3 + a2
    }
}
fn ease_in_bounce(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    a3 - ease_out_bounce(a4 - a1, 0.0, a3, a4) + a2
}
fn ease_in_out_bounce(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    let d = a1 + a1;
    if a1 >= 0.5 * a4 {
        ease_out_bounce(d - a4, 0.0, a3, a4) * 0.5 + 0.5 * a3 + a2
    } else {
        (a3 - ease_out_bounce(a4 - d, 0.0, a3, a4)) * 0.5 + a2
    }
}
fn ease_out_in_bounce(a1: f64, a2: f64, a3: f64, a4: f64) -> f64 {
    if a1 >= a4 * 0.5 {
        0.5 * a3 - ease_out_bounce(a4 - (a1 + a1 - a4), 0.0, 0.5 * a3, a4) + 0.5 * a3 + a2
    } else {
        ease_out_bounce(a1 + a1, a2, 0.5 * a3, a4)
    }
}
