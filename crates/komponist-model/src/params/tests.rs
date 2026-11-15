use super::*;
use crate::format::MINUS;

fn float(min: f64, max: f64, scale: Scale, unit: Unit) -> ParamInfo {
    ParamInfo::new("x", "X", ParamKind::scaled(min, max, scale), min).unit(unit)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6 * b.abs().max(1.0)
}

#[test]
fn value_texts_carry_the_words_of_a_language() {
    const EN: ValueTexts = ValueTexts {
        on: "On",
        off: "Off",
        percent: "%",
    };
    let switch = ParamInfo::new("s", "S", ParamKind::Bool, 0.0);
    let percent = ParamInfo::new("p", "P", ParamKind::lin(0.0, 1.0), 0.0).unit(Unit::Percent);
    // German is the default, as the model always wrote.
    assert_eq!(ValueTexts::default(), ValueTexts::DE);
    assert_eq!(
        (switch.format(1.0), switch.format(0.0)),
        ("Ein".into(), "Aus".into())
    );
    assert_eq!(percent.format(0.4), "40 %");
    let (switch, percent) = (switch.texts(EN), percent.texts(EN));
    assert_eq!(
        (switch.format(1.0), switch.format(0.0)),
        ("On".into(), "Off".into())
    );
    assert_eq!(percent.format(0.4), "40%");
    // Units and notation do not depend on the language.
    for texts in [ValueTexts::DE, EN] {
        let hz = float(20.0, 20_000.0, Scale::Log, Unit::Hz).texts(texts);
        assert_eq!(hz.format(1200.0), "1.2 kHz");
        let db = float(-12.0, 12.0, Scale::Lin, Unit::Db).texts(texts);
        assert_eq!(db.format(-4.0), format!("{MINUS}4.0 dB"));
        let pan = float(-1.0, 1.0, Scale::Pan, Unit::None).texts(texts);
        assert_eq!(pan.format(0.0), "C");
    }
}

#[test]
fn a_captured_format_belongs_to_one_info_and_its_clones() {
    let off = "Off";
    let a = float(0.0, 1.0, Scale::Lin, Unit::None).formatted(move |v| {
        if v < 0.1 {
            off.to_owned()
        } else {
            format!("{v}")
        }
    });
    assert_eq!(a.format(0.0), "Off");
    assert_eq!(a.clone(), a);
    // A separately built function is a different one, even with the same text.
    let b = float(0.0, 1.0, Scale::Lin, Unit::None).formatted(move |v| {
        if v < 0.1 {
            off.to_owned()
        } else {
            format!("{v}")
        }
    });
    assert_ne!(a, b);
}

#[test]
fn lin_log_and_pan_round_trip_and_hit_the_ends() {
    for p in [
        float(0.0, 1.0, Scale::Lin, Unit::None),
        float(20.0, 20_000.0, Scale::Log, Unit::Hz),
        float(0.001, 10.0, Scale::Log, Unit::S),
        float(-1.0, 1.0, Scale::Pan, Unit::None),
    ] {
        let (min, max) = p.range();
        assert_eq!(p.to_normalized(min), 0.0);
        assert!(close(p.to_normalized(max), 1.0));
        assert!(close(p.to_plain(0.0), min) && close(p.to_plain(1.0), max));
        for n in [0.0, 0.1, 0.5, 0.9, 1.0] {
            assert!(close(p.to_normalized(p.to_plain(n)), n), "{:?} {n}", p.kind);
        }
    }
    // Log: the geometric middle of 20 Hz … 20 kHz is 632 Hz.
    let f = float(20.0, 20_000.0, Scale::Log, Unit::Hz);
    assert!(close(f.to_plain(0.5), (20.0f64 * 20_000.0).sqrt()));
    // Pan is centred.
    assert_eq!(
        float(-1.0, 1.0, Scale::Pan, Unit::None).to_normalized(0.0),
        0.5
    );
}

#[test]
fn log_without_a_positive_minimum_falls_back_to_linear() {
    let p = float(0.0, 10.0, Scale::Log, Unit::None);
    assert_eq!(p.to_normalized(5.0), 0.5);
    assert_eq!(p.to_plain(0.25), 2.5);
}

#[test]
fn db_follows_the_fader_taper() {
    let p = float(-70.0, 6.0, Scale::Db, Unit::Db);
    assert_eq!(p.to_normalized(6.0), 1.0);
    assert_eq!(p.to_normalized(-70.0), 0.0);
    // 0 dB sits at taper position 0.14 from the top.
    assert!(close(p.to_normalized(0.0), 0.86));
    assert!(close(p.to_normalized(-12.0), 0.55));
    assert!(close(p.to_plain(0.86), 0.0));
    assert!(
        close(p.to_plain(0.0), -70.0),
        "the bottom is the minimum, not −∞"
    );
    for db in [-60.0, -24.0, -6.0, 0.0, 3.0] {
        assert!((p.to_plain(p.to_normalized(db)) - db).abs() < 1e-3, "{db}");
    }
    // A narrower range stretches the same taper over 0 … 1.
    let t = float(-24.0, 0.0, Scale::Db, Unit::Db);
    assert_eq!(t.to_normalized(-24.0), 0.0);
    assert!(close(t.to_normalized(0.0), 1.0));
    assert!((t.to_plain(t.to_normalized(-12.0)) + 12.0).abs() < 1e-3);
    // Outside the taper: linear.
    assert_eq!(
        float(-24.0, 24.0, Scale::Db, Unit::Db).to_normalized(0.0),
        0.5
    );
}

#[test]
fn stepped_kinds_snap_and_clamp() {
    let ratio = ParamInfo::new(
        "ratio2",
        "Ratio 2",
        ParamKind::Float {
            min: 0.0,
            max: 32.0,
            scale: Scale::Lin,
            steps: 64,
        },
        1.0,
    );
    assert_eq!(ratio.snap(3.3), 3.5);
    assert_eq!(ratio.snap(40.0), 32.0);
    assert_eq!(ratio.snap(f64::NAN), 1.0);
    assert_eq!(ratio.to_plain(ratio.to_normalized(7.2)), 7.0);
    let algo = ParamInfo::new(
        "algorithm",
        "Algorithmus",
        ParamKind::Int { min: 0, max: 32 },
        0.0,
    );
    assert_eq!(
        (algo.snap(7.4), algo.snap(-3.0), algo.steps()),
        (7.0, 0.0, 32)
    );
    assert_eq!(algo.to_plain(0.5), 16.0);
    let e = ParamInfo::new(
        "mode",
        "Modus",
        ParamKind::Enum(vec!["A".into(), "B".into(), "C".into()]),
        0.0,
    );
    assert_eq!((e.range(), e.steps()), ((0.0, 2.0), 2));
    assert_eq!(e.to_plain(0.74), 1.0);
    assert_eq!(e.format(2.0), "C");
    let b = ParamInfo::new("on", "An", ParamKind::Bool, 0.0);
    assert_eq!(
        (b.to_plain(0.6), b.format(0.0), b.format(1.0)),
        (1.0, "Aus".into(), "Ein".into())
    );
    // A degenerate range does not divide by zero.
    let d = float(1.0, 1.0, Scale::Log, Unit::None);
    assert_eq!((d.to_normalized(1.0), d.to_plain(0.5)), (0.0, 1.0));
}

#[test]
fn texts_per_unit() {
    let hz = float(20.0, 20_000.0, Scale::Log, Unit::Hz);
    assert_eq!(hz.format(420.0), "420 Hz");
    assert_eq!(hz.format(999.4), "999 Hz");
    assert_eq!(hz.format(1200.0), "1.2 kHz");
    assert_eq!(hz.format(20_000.0), "20 kHz");
    let db = float(-70.0, 6.0, Scale::Db, Unit::Db);
    assert_eq!(db.format(-4.0), format!("{MINUS}4.0 dB"));
    assert_eq!(db.format(1.5), "+1.5 dB");
    assert_eq!(db.format(0.0), "0.0 dB");
    assert_eq!(db.format(-70.0), format!("{MINUS}\u{221e} dB"));
    let pan = float(-1.0, 1.0, Scale::Pan, Unit::None);
    assert_eq!(pan.format(-0.28), "L 14");
    assert_eq!(pan.format(0.0), "C");
    assert_eq!(pan.format(1.0), "R 50");
    let ms = float(0.1, 200.0, Scale::Log, Unit::Ms);
    assert_eq!(
        (ms.format(8.0).as_str(), ms.format(120.0).as_str()),
        ("8.0 ms", "120 ms")
    );
    assert_eq!(
        float(1.0, 2000.0, Scale::Log, Unit::Ms).format(1500.0),
        "1.50 s"
    );
    let s = float(0.001, 20.0, Scale::Log, Unit::S);
    assert_eq!(s.format(0.3), "300 ms");
    assert_eq!(s.format(1.2), "1.20 s");
    assert_eq!(s.format(12.0), "12.0 s");
    assert_eq!(
        float(0.0, 1.0, Scale::Lin, Unit::Percent).format(0.4),
        "40 %"
    );
    assert_eq!(
        float(-48.0, 48.0, Scale::Lin, Unit::Semitones).format(7.0),
        "+7 st"
    );
    assert_eq!(
        float(-100.0, 100.0, Scale::Lin, Unit::Cents).format(-12.0),
        format!("{MINUS}12 ct")
    );
    assert_eq!(
        float(1.0, 20.0, Scale::Log, Unit::Ratio).format(4.0),
        "4.0:1"
    );
    assert_eq!(
        float(1.0, 60.0, Scale::Log, Unit::Meters).format(28.0),
        "28 m"
    );
    assert_eq!(
        float(0.1, 18.0, Scale::Log, Unit::None).format(0.71),
        "0.71"
    );
    assert_eq!(
        float(-1.0, 1.0, Scale::Lin, Unit::None).format(-0.5),
        format!("{MINUS}0.50")
    );
    let n = ParamInfo::new("n", "N", ParamKind::Int { min: -3, max: 3 }, 0.0);
    assert_eq!(n.format(2.0), "2");
    assert_eq!(n.format(-1.0), format!("{MINUS}1"));
}

#[test]
fn an_own_format_wins() {
    fn frei(v: f64) -> String {
        if v < 0.5 {
            "Frei".into()
        } else {
            format!("{v}")
        }
    }
    let p = ParamInfo::new(
        "algorithm",
        "Algorithmus",
        ParamKind::Int { min: 0, max: 32 },
        0.0,
    )
    .formatted(frei);
    assert_eq!(p.format(0.0), "Frei");
    assert_eq!(p.format(5.0), "5");
}

#[test]
fn ids_are_stable_strings() {
    assert_eq!(ParamId::plugin(42).as_str(), "p42");
    assert_eq!(ParamId::plugin(42).plugin_id(), Some(42));
    for other in ["op1_attack", "p", "p4x", "pad1_tune", "P4"] {
        assert_eq!(ParamId::new(other).plugin_id(), None, "{other}");
    }
    assert_eq!(
        serde_json::to_string(&ParamId::new("b1_freq")).unwrap(),
        "\"b1_freq\""
    );
    let back: ParamId = serde_json::from_str("\"b1_freq\"").unwrap();
    assert_eq!(back, ParamId::from("b1_freq"));
}

#[test]
fn info_round_trips_through_json_without_the_format_fn() {
    let p = float(20.0, 20_000.0, Scale::Log, Unit::Hz).formatted(|_| "x".into());
    let json = serde_json::to_string(&p).unwrap();
    let back: ParamInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(back.format, None);
    assert_eq!(back, ParamInfo { format: None, ..p });
}

#[test]
fn placeholders_resolve_from_the_context() {
    let ctx = ParamContext::new()
        .with(context::ENV_OP, 3)
        .with(context::BAND, 7)
        .with(context::SEL, 2)
        .with(context::ENV_VIEW, "filt");
    let id = |t: &str| resolve(t, &ctx).map(|i| i.to_string());
    assert_eq!(id("op{env_op}_attack").unwrap(), "op3_attack");
    assert_eq!(id("b{band}_freq").unwrap(), "b7_freq");
    assert_eq!(id("pad{sel}_tune").unwrap(), "pad2_tune");
    assert_eq!(id("{env_view}_a").unwrap(), "filt_a");
    assert_eq!(id("op{env_op}_to_{band}").unwrap(), "op3_to_7");
    assert_eq!(id("volume").unwrap(), "volume");
    assert_eq!(
        placeholders("op{env_op}_{band}").unwrap(),
        ["env_op", "band"]
    );
    assert!(placeholders("volume").unwrap().is_empty());
    assert!(context::is_template("b{band}_q") && !context::is_template("b1_q"));
}

#[test]
fn bad_templates_are_errors() {
    let ctx = ParamContext::new().with(context::BAND, 1);
    assert_eq!(
        resolve("op{env_op}_attack", &ctx),
        Err(ResolveError::Missing("env_op".into()))
    );
    for bad in ["b{band", "b}band{", "b{}_q", "b{{band}}", "b{ba{nd}"] {
        assert_eq!(resolve(bad, &ctx), Err(ResolveError::Malformed), "{bad}");
    }
    let mut ctx = ctx;
    ctx.set(context::BAND, 8);
    assert_eq!(ctx.get(context::BAND), Some("8"));
    assert_eq!(resolve("b{band}_q", &ctx).unwrap().as_str(), "b8_q");
}
