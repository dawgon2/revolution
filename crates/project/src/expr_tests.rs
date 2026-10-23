//! Expression language tests (C4.2): oracle values, caps and hostile input.

use super::*;
use filmcraft_time::TICKS_PER_SECOND;
use proptest::prelude::*;

fn cx() -> Context {
    // 1.5 s into a 24 fps 1920×1080 comp, value 10
    Context::new(Tick(TICKS_PER_SECOND * 3 / 2), FrameRate::FPS_24, 1920, 1080, Value::Scalar(10.0))
}

fn ev(src: &str) -> Result<Value> {
    eval_str(src, &cx(), &NoParams)
}

fn num(src: &str) -> f64 {
    match ev(src) {
        Ok(Value::Scalar(x)) => x,
        other => panic!("{src}: {other:?}"),
    }
}

fn vect(src: &str) -> Vec<f64> {
    match ev(src) {
        Ok(v @ Value::Vector { .. }) => v.components().to_vec(),
        other => panic!("{src}: {other:?}"),
    }
}

fn err(src: &str) -> ExprError {
    match ev(src) {
        Err(e) => e,
        Ok(v) => panic!("{src} should fail, gave {v:?}"),
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn arithmetic_and_precedence() {
    for (src, want) in [
        ("1+2*3", 7.0),
        ("(1+2)*3", 9.0),
        ("10-2-3", 5.0),
        ("8/2/2", 2.0),
        ("2^3^2", 512.0),
        ("-2^2", -4.0),
        ("2^-1", 0.5),
        ("7%3", 1.0),
        ("-7%3", -1.0),
        ("1e3", 1000.0),
        (".5", 0.5),
        ("2.5e-1", 0.25),
        ("+3", 3.0),
        ("--3", 3.0),
        ("1<2", 1.0),
        ("2<=1", 0.0),
        ("3>=3", 1.0),
        ("3>4", 0.0),
        ("1==1", 1.0),
        ("1!=1", 0.0),
        ("1<2 && 2<1", 0.0),
        ("0 || 3", 1.0),
        ("!0", 1.0),
        ("!5", 0.0),
        ("1 ? 2 : 3", 2.0),
        ("0 ? 2 : 0 ? 3 : 4", 4.0),
        ("1 + 2 > 2 ? 10 : 20", 10.0),
        ("  1 +\t2\n", 3.0),
        ("2*-3", -6.0),
    ] {
        assert!(close(num(src), want), "{src} = {} (want {want})", num(src));
    }
    // short-circuit: the right side isn't evaluated
    assert_eq!(num("0 && 1/0"), 0.0);
    assert_eq!(num("1 || 1/0"), 1.0);
    assert_eq!(num("1 ? 5 : 1/0"), 5.0);
}

#[test]
fn vectors() {
    assert_eq!(vect("[1,2]+[3,4]"), vec![4.0, 6.0]);
    assert_eq!(vect("[1,2]*2"), vec![2.0, 4.0]);
    assert_eq!(vect("10-[1,2,3]"), vec![9.0, 8.0, 7.0]);
    assert_eq!(vect("-[1,2]"), vec![-1.0, -2.0]);
    assert_eq!(vect("[1,2,3,4]/2"), vec![0.5, 1.0, 1.5, 2.0]);
    assert_eq!(num("[1,2,3][1]"), 2.0);
    assert_eq!(num("[1,2,3][2.7]"), 3.0);
    assert_eq!(num("[1,2,3,4].w"), 4.0);
    assert_eq!(num("[1,2].y"), 2.0);
    assert_eq!(num("[0.5,0.25,1].g"), 0.25);
    assert_eq!(num("[0.5,0.25,1,0.3].a"), 0.3);
    assert_eq!(num("(5).x"), 5.0);
    assert_eq!(num("5[0]"), 5.0);
    assert_eq!(num("[7]"), 7.0);
    assert_eq!(vect("[1,2] < [2,1]"), vec![1.0, 0.0]);
    assert_eq!(vect("value + [1, 2]"), vec![11.0, 12.0]);
    for bad in ["[1,2]+[1,2,3]", "[[1,2],3]", "[1,2][2]", "[1,2][-1]", "[1,2] ? 1 : 0", "(5).y", "[1,2][[0,1]]", "![1,2]", "[1,2] && 1"] {
        let e = err(bad);
        assert!(!e.message.is_empty(), "{bad}");
    }
    assert!(parse("[1,2,3,4,5]").is_err());
    assert!(parse("[]").is_err());
    assert!(parse("[1,2].q").is_err());
}

#[test]
fn names() {
    assert!(close(num("time"), 1.5));
    assert_eq!(num("frame"), 36.0);
    assert_eq!(num("fps"), 24.0);
    assert_eq!(num("width"), 1920.0);
    assert_eq!(num("height"), 1080.0);
    assert_eq!(num("value"), 10.0);
    assert!(close(num("pi"), std::f64::consts::PI));
    assert!(close(num("e"), std::f64::consts::E));
    assert!(close(num("time*10"), 15.0));
}

#[test]
fn every_function() {
    for (src, want) in [
        ("sin(pi/2)", 1.0),
        ("cos(0)", 1.0),
        ("tan(0)", 0.0),
        ("asin(1)", std::f64::consts::FRAC_PI_2),
        ("acos(1)", 0.0),
        ("atan(1)", std::f64::consts::FRAC_PI_4),
        ("atan2(1, 1)", std::f64::consts::FRAC_PI_4),
        ("sqrt(16)", 4.0),
        ("abs(-3)", 3.0),
        ("sign(-3)", -1.0),
        ("sign(0)", 0.0),
        ("sign(2)", 1.0),
        ("min(3, 1, 2)", 1.0),
        ("max(3, 1, 2)", 3.0),
        ("clamp(5, 0, 1)", 1.0),
        ("clamp(-5, 0, 1)", 0.0),
        ("clamp(0.5, 1, 0)", 0.5),
        ("floor(1.7)", 1.0),
        ("floor(-1.2)", -2.0),
        ("ceil(1.2)", 2.0),
        ("round(1.5)", 2.0),
        ("fract(1.25)", 0.25),
        ("fract(-0.25)", 0.75),
        ("exp(0)", 1.0),
        ("ln(e)", 1.0),
        ("log10(1000)", 3.0),
        ("pow(2, 10)", 1024.0),
        ("lerp(0, 10, 0.25)", 2.5),
        ("smoothstep(0, 1, 0.5)", 0.5),
        ("smoothstep(0, 1, 2)", 1.0),
        ("smoothstep(1, 1, 2)", 1.0),
        ("step(0.5, 0.7)", 1.0),
        ("step(0.5, 0.2)", 0.0),
        ("noise(3)", 0.0),
        ("noise(3, 7)", 0.0),
    ] {
        assert!(close(num(src), want), "{src} = {} (want {want})", num(src));
    }
    assert_eq!(vect("abs([-1, 2])"), vec![1.0, 2.0]);
    assert_eq!(vect("max([1, 5], 3)"), vec![3.0, 5.0]);
    assert_eq!(vect("lerp([0, 0], [10, 20], 0.5)"), vec![5.0, 10.0]);
    let r = num("random(1)");
    assert!((0.0..1.0).contains(&r));
    assert_eq!(r, num("random(1)"), "random is deterministic");
    assert_ne!(r, num("random(2)"));
    // noise: deterministic, in −1..1, varies between integers, seeds differ
    let n = num("noise(3.4)");
    assert!((-1.0..=1.0).contains(&n) && n != 0.0, "{n}");
    assert_eq!(n, num("noise(3.4)"));
    assert_ne!(n, num("noise(3.4, 1)"));
    // wiggle = value + amp × noise(time × freq, seed)
    assert!(close(num("wiggle(2.3, 5)"), 10.0 + 5.0 * noise(1.5 * 2.3, 0)));
    assert!(close(num("wiggle(2.3, 5, 9)"), 10.0 + 5.0 * noise(1.5 * 2.3, seed_bits(9.0))));
    let v = eval_str("wiggle(2.3, 5)", &Context { value: Value::from_slice(&[1.0, 1.0]), ..cx() }, &NoParams).unwrap();
    let c = v.components();
    assert_eq!(c.len(), 2);
    assert_ne!(c[0], c[1], "each component wiggles on its own");
    // arities and domains
    for bad in [
        "sin()",
        "sin(1, 2)",
        "atan2(1)",
        "clamp(1, 2)",
        "min(1)",
        "noise()",
        "wiggle(1)",
        "min(1,2,3,4,5,6,7,8,9)",
        "sqrt(-1)",
        "ln(0)",
        "log10(-1)",
        "asin(2)",
        "acos(-1.5)",
        "pow(-8, 1/3)",
        "pow(0, -1)",
        "pow(10, 400)",
        "exp(1000)",
        "wiggle([1,2], 1)",
    ] {
        assert!(ev(bad).is_err(), "{bad} should fail");
    }
    for f in Func::ALL {
        assert_eq!(Func::from_name(f.name()), Some(f));
    }
}

#[test]
fn noise_stays_in_range_and_is_continuous() {
    let mut prev = noise(0.0, 0);
    for i in 1..20_000 {
        let x = f64::from(i) * 0.001 - 3.0;
        let n = noise(x, 42);
        assert!((-1.0..=1.0).contains(&n), "{x}: {n}");
        if i > 1 {
            assert!((n - prev).abs() < 0.02, "jump at {x}");
        }
        prev = n;
    }
    for x in [f64::NAN, f64::INFINITY, -1e300, 1e300, 9.0e15 + 0.5] {
        assert!(noise(x, 1).is_finite());
    }
    for s in [0.0, -0.0, 1e308, -5.5] {
        assert!((0.0..1.0).contains(&random(s)));
    }
    assert_eq!(random(0.0), random(-0.0));
}

/// `frame` comes exactly from the tick and the rate: at NTSC rates every frame's first tick is
/// that frame and the tick before it the previous one.
#[test]
fn frame_is_exact_at_ntsc_rates() {
    for rate in [FrameRate::FPS_23_976, FrameRate::FPS_29_97] {
        for f in [0i64, 1, 2, 999, 1000, 1001, 24_000, 86_313, 1_000_000] {
            let t = rate.tick_of(f);
            let at = Context::new(t, rate, 1, 1, Value::Scalar(0.0));
            assert_eq!(at.frame, f, "{rate:?} frame {f}");
            assert_eq!(eval_str("frame", &at, &NoParams), Ok(Value::Scalar(f as f64)));
            if f > 0 {
                assert_eq!(Context::new(Tick(t.0 - 1), rate, 1, 1, Value::Scalar(0.0)).frame, f - 1);
            }
        }
    }
    let c = Context::new(Tick::ZERO, FrameRate { num: 0, den: 0 }, 1, 1, Value::Scalar(0.0));
    assert!(c.fps.is_finite() && c.fps > 0.0, "a bad rate is the default rate");
    assert!(close(Context::new(Tick::ZERO, FrameRate::FPS_29_97, 1, 1, Value::Scalar(0.0)).fps, 30000.0 / 1001.0));
}

#[test]
fn coercions() {
    let like_f = ParamValue::Float(0.0);
    assert_eq!(to_param(Value::Scalar(2.5), &like_f, None), Ok(ParamValue::Float(2.5)));
    assert_eq!(to_param(Value::from_slice(&[3.0, 4.0]), &like_f, None), Ok(ParamValue::Float(3.0)));
    let like_p = ParamValue::Vec2(Vec2::ZERO);
    assert_eq!(to_param(Value::from_slice(&[3.0, 4.0]), &like_p, None), Ok(ParamValue::Vec2(Vec2::new(3.0, 4.0))));
    assert_eq!(to_param(Value::Scalar(7.0), &like_p, None), Ok(ParamValue::Vec2(Vec2::new(7.0, 7.0))));
    assert!(to_param(Value::from_slice(&[1.0, 2.0, 3.0]), &like_p, None).is_err());
    assert!(to_param(Value::from_slice(&[1.0, 2.0, 3.0, 4.0]), &like_p, None).is_err());
    let like_c = ParamValue::Color([0.0, 0.0, 0.0, 0.5]);
    assert_eq!(to_param(Value::from_slice(&[1.0, 0.5, 0.25, 1.0]), &like_c, None), Ok(ParamValue::Color([1.0, 0.5, 0.25, 1.0])));
    assert_eq!(to_param(Value::from_slice(&[1.0, 0.5, 0.25]), &like_c, None), Ok(ParamValue::Color([1.0, 0.5, 0.25, 0.5])));
    assert_eq!(to_param(Value::Scalar(0.2), &like_c, None), Ok(ParamValue::Color([0.2, 0.2, 0.2, 0.5])));
    assert!(to_param(Value::from_slice(&[1.0, 0.5]), &like_c, None).is_err());
    assert!(to_param(Value::Scalar(1e300), &like_c, None).is_err(), "too large for f32");
    let like_b = ParamValue::Bool(false);
    assert_eq!(to_param(Value::Scalar(0.0), &like_b, None), Ok(ParamValue::Bool(false)));
    assert_eq!(to_param(Value::Scalar(-0.1), &like_b, None), Ok(ParamValue::Bool(true)));
    let like_ch = ParamValue::Choice(0);
    assert_eq!(to_param(Value::Scalar(1.6), &like_ch, Some(3)), Ok(ParamValue::Choice(2)));
    assert_eq!(to_param(Value::Scalar(99.0), &like_ch, Some(3)), Ok(ParamValue::Choice(2)));
    assert_eq!(to_param(Value::Scalar(-5.0), &like_ch, Some(3)), Ok(ParamValue::Choice(0)));
    assert_eq!(to_param(Value::Scalar(1e20), &like_ch, None), Ok(ParamValue::Choice(u32::MAX)));
    assert_eq!(to_param(Value::Scalar(4.0), &like_ch, Some(0)), Ok(ParamValue::Choice(0)));
    assert!(to_param(Value::Scalar(1.0), &ParamValue::Text("x".into()), None).is_err());
    assert!(to_param(Value::Scalar(f64::NAN), &like_f, None).is_err());

    assert_eq!(from_param(&ParamValue::Float(2.0)), Some(Value::Scalar(2.0)));
    assert_eq!(from_param(&ParamValue::Choice(3)), Some(Value::Scalar(3.0)));
    assert_eq!(from_param(&ParamValue::Bool(true)), Some(Value::Scalar(1.0)));
    assert_eq!(from_param(&ParamValue::Vec2(Vec2::new(1.0, 2.0))), Some(Value::from_slice(&[1.0, 2.0])));
    assert_eq!(from_param(&ParamValue::Color([1.0, 0.0, 0.5, 1.0])), Some(Value::from_slice(&[1.0, 0.0, 0.5, 1.0])));
    assert_eq!(from_param(&ParamValue::Text("x".into())), None);
    assert_eq!(from_param(&ParamValue::Curve(Vec::new())), None);
    assert!(kind_allowed(&ParamValue::Float(0.0)) && kind_allowed(&ParamValue::Choice(0)));
    assert!(!kind_allowed(&ParamValue::Text(String::new())) && !kind_allowed(&ParamValue::Curve(Vec::new())));
}

#[test]
fn errors_say_what_and_where() {
    let e = err("sin(time");
    assert_eq!(e.message, "expected `)` after `sin(time`");
    assert_eq!(e.column, 9);
    assert_eq!(e.to_string(), "expected `)` after `sin(time` at column 9");
    assert!(err("tim").message.contains("unknown name `tim`"));
    assert!(err("sine(1)").message.contains("unknown function `sine`"));
    assert!(err("sin").message.contains("is a function"));
    assert!(err("x = 1").message.contains("=="));
    assert!(err("1 = 1").message.contains("=="));
    assert!(err("1 +").message.contains("right"));
    assert!(err("1 2").message.contains("unexpected `2`"));
    assert!(err("1/0").message.contains("division by zero"));
    assert!(err("1%0").message.contains("division by zero"));
    // columns count characters, not bytes
    let e = err("\"é\" + é");
    assert!(e.column > 0);
    let e = err("é");
    assert_eq!((e.span, e.column), (Span::new(0, 2), 1));
    assert!(err("").message.contains("empty"));
    assert!(err("   ").message.contains("empty"));
    assert!(err("\"open").message.contains("quote"));
    assert!(err("self.opacity").message.contains("opacity"), "no resolver: an error, not a panic");
    assert!(err("node(\"a\").opacity").message.contains("opacity"));
    assert!(parse("node(a).x").is_err());
    assert!(parse("node(-1).x").is_err());
    assert!(parse("node(1.5).x").is_err());
    assert!(parse("node(\"a\")").is_err());
    assert!(parse("node(3).opacity").is_ok());
    assert!(parse("self").is_err());
}

#[test]
fn non_finite_results_are_errors() {
    for bad in ["1e308*10", "1e308^9", "0/0", "1e999", "-1e308-1e308", "exp(710)", "[1e308, 1]*10", "tan(pi/2)*1e300"] {
        assert!(ev(bad).is_err(), "{bad}");
    }
    let nan_value = Context { value: Value::Scalar(f64::NAN), ..cx() };
    assert!(eval_str("value", &nan_value, &NoParams).is_err());
    assert!(eval_str("value * 0", &nan_value, &NoParams).is_err());
}

// ---------------------------------------------------------------------------------------------
// hostile input: never panic, always stop within the caps

#[test]
fn hostile_nesting_and_size() {
    // 10k nested parentheses (over the size cap)
    let deep = format!("{}1{}", "(".repeat(10_000), ")".repeat(10_000));
    assert!(err(&deep).message.contains("longer"));
    // within the size cap, over the nesting cap
    let deep = format!("{}1{}", "(".repeat(2_000), ")".repeat(2_000));
    assert!(err(&deep).message.contains("nested"));
    let deep = format!("{}1", "-".repeat(4_000));
    assert!(err(&deep).message.contains("nested"));
    let deep = format!("{}1", "[".repeat(4_000));
    assert!(ev(&deep).is_err());
    let deep = "sin(".repeat(1_000);
    assert!(ev(&deep).is_err());
    let deep = "1?".repeat(2_000) + "1";
    assert!(ev(&deep).is_err());
    let deep = "2^".repeat(2_000) + "1";
    assert!(ev(&deep).is_err());
    // nesting right at the cap is fine
    let ok = format!("{}1{}", "(".repeat(60), ")".repeat(60));
    assert_eq!(num(&ok), 1.0);
    // 4 KB of `1+1+…`: over the node or height cap, an error
    let long = "1+".repeat(2_047) + "1";
    assert!(long.len() <= MAX_SOURCE);
    assert!(ev(&long).is_err());
    // a long chain within the caps evaluates
    let chain = "1+".repeat(200) + "1";
    assert_eq!(num(&chain), 201.0);
    // the node cap with a shallow tree
    let wide = (0..300).map(|_| "[1,1]").collect::<Vec<_>>().join("+");
    assert!(wide.len() <= MAX_SOURCE);
    assert!(err(&wide).message.contains("parts") || err(&wide).message.contains("chain"));
    // the height cap holds the evaluator's recursion
    let tall = (0..MAX_HEIGHT as usize + 5).fold("1".to_string(), |a, _| format!("({a})+1"));
    if tall.len() <= MAX_SOURCE {
        assert!(ev(&tall).is_err());
    }
}

#[test]
fn hostile_garbage() {
    for src in [
        "",
        "\0",
        "🙂",
        "sin(🙂)",
        "\"🙂\"",
        "node(\"🙂\").x",
        "１+１",
        "\u{202e}1+1",
        "1 +* 2",
        "((((",
        "))))",
        "[,]",
        "[1,]",
        "?:",
        "1 ? 2",
        "1 ? : 3",
        "a.b.c.d",
        "1..2",
        "1.2.3",
        "1e",
        "1e+",
        "0x10",
        "time(",
        "frame[",
        "value.",
        ".",
        "self.",
        "node(",
        "node(\"a\"",
        "node(\"a\").",
        "min(,)",
        "&&",
        "||1",
        "!",
        "'",
    ] {
        let _ = ev(src);
        let _ = parse(src);
    }
}

/// A resolver that answers every reference with a vector (for fuzzing evaluation paths).
struct AnyParam;

impl Resolver for AnyParam {
    fn param(&self, _: Option<&str>, key: &str, _: u32) -> Result<Value> {
        Ok(Value::from_slice(&[key.len() as f64, 2.0]))
    }
}

const SOUP: &[&str] = &[
    "1",
    "0",
    "2.5",
    "1e308",
    "-",
    "+",
    "*",
    "/",
    "%",
    "^",
    "!",
    "(",
    ")",
    "[",
    "]",
    ",",
    ".",
    "?",
    ":",
    "<",
    ">",
    "<=",
    "==",
    "!=",
    "&&",
    "||",
    "time",
    "frame",
    "value",
    "pi",
    "x",
    "y",
    "w",
    "self.opacity",
    "node(\"a\").b",
    "sin(",
    "min(",
    "max(",
    "clamp(",
    "wiggle(",
    "noise(",
    "random(",
    "pow(",
    "lerp(",
    "sqrt(",
    "ln(",
    "[1,2]",
    "[1,2,3,4]",
    " ",
    "\"",
    "é",
    "🙂",
];

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_000, ..ProptestConfig::default() })]

    #[test]
    fn fuzz_random_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let src = String::from_utf8_lossy(&bytes);
        let _ = eval_str(&src, &cx(), &AnyParam);
    }

    #[test]
    fn fuzz_token_soup(picks in proptest::collection::vec(0..SOUP.len(), 0..300)) {
        let src: String = picks.iter().filter_map(|&i| SOUP.get(i).copied()).collect();
        if let Ok(e) = parse(&src) {
            prop_assert!(e.height() <= MAX_HEIGHT);
            if let Ok(v) = eval(&e, &cx(), &AnyParam) {
                prop_assert!(v.is_finite());
            }
        }
    }

    #[test]
    fn fuzz_arithmetic_is_finite_or_an_error(a in any::<f64>(), b in any::<f64>(), op in 0usize..6) {
        let ops = ["+", "-", "*", "/", "%", "^"];
        let cx = Context { value: Value::from_slice(&[a, b]), ..cx() };
        let src = format!("value.x {} value.y", ops.get(op).copied().unwrap_or("+"));
        if let Ok(v) = eval_str(&src, &cx, &NoParams) {
            prop_assert!(v.is_finite());
        }
    }
}
