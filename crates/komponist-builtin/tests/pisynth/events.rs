//! Sample-accurate events: notes, note-offs, All Notes Off, reset, macros.

use komponist_builtin::{Macro, Patch, PisynthProcessor, default_patch, preset_patch};
use komponist_engine::{EventKind, Processor, ProcessorError};

use crate::common::*;

/// Init, E-Piano (stereo, six operators), FM-Bass.
fn patches() -> [Patch; 3] {
    [0, 1, 35].map(|i| preset_patch(i).unwrap())
}

#[test]
fn notes_sound_from_their_frame_plus_the_latency() {
    for patch in patches() {
        let latency = activated(patch).latency() as usize;
        assert_eq!(latency, 17, "2× oversampling filter");
        for block in [64, 256, 1000] {
            for at in [0, 1, 17, 31, 32, 33, 63, 100, 255, 999, 1500] {
                let mut p = activated(patch);
                let out = play(&mut p, at + 400, block, &[(at, note_on(60))]);
                // Nothing before the note's frame; its first (filter) sample is on it.
                assert_eq!(out.first_nonzero(), Some(at), "block {block}, note at {at}");
                // Sound (−100 dBFS) after the latency, ±1.
                let onset = out.onset(AUDIBLE).expect("the note sounds");
                assert!(
                    onset.abs_diff(at + latency) <= 1,
                    "block {block}, note at {at}: onset {onset}"
                );
                assert!(out.peak(at..at + 400) > 0.05);
            }
        }
    }
}

#[test]
fn a_second_note_changes_the_output_from_its_frame_on() {
    for patch in patches() {
        for at in [40, 64, 200, 511] {
            // Same splits in both runs; only the second note differs.
            let mut one = activated(patch);
            let mut two = activated(patch);
            let alone = play(&mut one, 2048, 512, &[(10, note_on(60)), (at, nothing())]);
            let both = play(&mut two, 2048, 512, &[(10, note_on(60)), (at, note_on(67))]);
            // Nothing changes before the second note's frame. Its first filter samples (pre-
            // ringing, 1e-10 … 1e-9) may vanish in the rounding of the sum.
            let first = alone
                .first_difference(&both)
                .expect("the second note sounds");
            assert!(
                (at..at + 17).contains(&first),
                "second note at {at}: {first}"
            );
            // The second note on its own (the difference) sounds after the latency, ±1.
            let added = Out {
                l: both.l.iter().zip(&alone.l).map(|(a, b)| a - b).collect(),
                r: both.r.iter().zip(&alone.r).map(|(a, b)| a - b).collect(),
            };
            let onset = added.onset(AUDIBLE).expect("audible");
            assert!(
                onset.abs_diff(at + 17) <= 1,
                "second note at {at}: onset {onset}"
            );
        }
    }
}

#[test]
fn note_off_releases_from_its_frame() {
    // Init: carrier sustain 0.7, release 0.3 s with the default exponential curve.
    let patch = default_patch();
    let off = frames(0.25) + 123;
    let mut held = activated(patch);
    let mut released = activated(patch);
    let held = play(
        &mut held,
        off + frames(0.5),
        256,
        &[(0, note_on(60)), (off, nothing())],
    );
    let total = off + frames(0.5);
    let out = play(
        &mut released,
        total,
        256,
        &[(0, note_on(60)), (off, note_off(60))],
    );
    // The release starts on the note-off frame from the held level, so the first sample that
    // differs is that frame or the next.
    let first = held
        .first_difference(&out)
        .expect("the note-off changes the output");
    assert!(
        (off..=off + 1).contains(&first),
        "note-off at {off}: {first}"
    );

    let before = out.rms(off - frames(0.02)..off);
    assert!(before > 0.05, "held note sounds: {before}");
    // A release, not a cut: still loud right after the note-off …
    let start = off + 17;
    assert!(out.rms(start..start + frames(0.005)) > 0.5 * before);
    // … falling well below by the end of the release …
    assert!(out.rms(off + frames(0.25)..off + frames(0.28)) < 0.02 * before);
    // … and silent after it, while the held note goes on.
    let end = off + frames(0.35);
    assert_eq!(released.active_voices(), 0);
    assert_eq!(out.peak(end..total), 0.0);
    assert!(held.rms(end..total) > 0.5 * before);
}

#[test]
fn all_notes_off_releases_keys_and_pedal() {
    let mut p = activated(preset_patch(1).unwrap());
    let pedal_down = EventKind::Midi([0xb0, 64, 127]);
    let mut events = vec![(0, pedal_down)];
    for (i, key) in [48, 55, 60, 64].into_iter().enumerate() {
        events.push((10 * i, note_on(key)));
        events.push((frames(0.1) + i, note_off(key)));
    }
    let out = play(&mut p, frames(0.3), 256, &events);
    // The pedal holds the released keys.
    assert_eq!(p.active_voices(), 4);
    assert!(out.rms(frames(0.25)..frames(0.3)) > 0.01);

    // All Notes Off lifts the pedal too: everything releases (E-Piano: release ≤ 0.6 s).
    let out = play(&mut p, frames(1.0), 256, &[(77, EventKind::AllNotesOff)]);
    assert!(out.rms(0..frames(0.01)) > 0.0, "a release, not a cut");
    assert_eq!(p.active_voices(), 0);
    assert_eq!(out.peak(frames(0.9)..frames(1.0)), 0.0);

    // The pedal stays up: a new note's note-off releases it.
    let out = play(
        &mut p,
        frames(1.2),
        256,
        &[(0, note_on(60)), (frames(0.1), note_off(60))],
    );
    assert!(out.peak(0..frames(0.1)) > 0.01);
    assert_eq!(p.active_voices(), 0);
}

#[test]
fn reset_silences_at_once_and_clears_effect_tails() {
    let mut patch = default_patch();
    patch.effects.delay.mix = 0.5;
    patch.effects.reverb.mix = 0.5;
    let mut p = activated(patch);
    let events = [
        (0, note_on(60)),
        (0, note_on(64)),
        (frames(0.2), note_off(60)),
        (frames(0.2), note_off(64)),
    ];
    let out = play(&mut p, frames(1.0), 256, &events);
    // Voices ended, the effects still ring.
    assert_eq!(p.active_voices(), 0);
    assert!(out.peak(frames(0.9)..frames(1.0)) > 1e-3);

    reset(&mut p);
    let after = play(&mut p, frames(1.0), 256, &[]);
    // Silent at once. Not bit-exact zero: the reverb is still on in the patch and restarts
    // from silence, with pisynth's anti-denormal offset (1e-20) in its tank.
    assert!(after.peak(0..after.len()) < 1e-12);
    assert_eq!(*p.patch(), patch.sanitized());

    // Reset in the middle of notes, then a new note plays on time.
    let _ = play(&mut p, 1000, 256, &[(0, note_on(50))]);
    reset(&mut p);
    assert_eq!(p.active_voices(), 0);
    let out = play(&mut p, 2000, 256, &[(300, note_on(62))]);
    assert!(
        out.onset(AUDIBLE)
            .is_some_and(|onset| onset.abs_diff(300 + 17) <= 1)
    );
}

#[test]
fn macros_apply_at_their_frame() {
    let patch = preset_patch(1).unwrap();
    for at in [1, 100, 257] {
        let mut a = activated(patch);
        let mut b = activated(patch);
        let plain = play(&mut a, 1024, 256, &[(0, note_on(60)), (at, nothing())]);
        let changed = play(
            &mut b,
            1024,
            256,
            &[(0, note_on(60)), (at, param(Macro::Index, 1.0))],
        );
        assert_eq!(plain.first_difference(&changed), Some(at), "Index at {at}");
        assert_eq!(b.patch().operators[1].level, 1.0);
    }
}

#[test]
fn macro_events_match_the_document_side_set() {
    let patch = preset_patch(1).unwrap();
    let changes = [
        (Macro::Algorithm, 7.0),
        (Macro::Ratio2, 3.5),
        (Macro::Index, 0.2),
        (Macro::Feedback, 0.6),
        (Macro::Attack, 0.3),
        (Macro::Decay, 0.5),
        (Macro::Sustain, 0.4),
        (Macro::Release, 0.45),
    ];
    // What the app does on its copy of the patch.
    let mut expected = patch;
    for (m, v) in changes {
        m.set(&mut expected, v);
    }
    // The same changes as events, all at one frame and spread over two calls.
    let mut p = activated(patch);
    let mut events: Vec<_> = changes[..5]
        .iter()
        .map(|&(m, v)| (100, param(m, v)))
        .collect();
    events.extend(changes[5..].iter().map(|&(m, v)| (300, param(m, v))));
    events.push((300, EventKind::Param { id: 999, value: 1.0 }));
    let _ = play(&mut p, 512, 256, &events);
    assert_eq!(*p.patch(), expected);
    for (m, v) in changes {
        assert!((m.get(p.patch()) - v).abs() < 1e-6, "{m:?}");
    }
}

#[test]
fn every_event_kind_and_reset_run_without_allocating() {
    // `call` and `reset` abort the test binary on any allocation.
    let mut p = activated(preset_patch(5).unwrap());
    let events = [
        (0, note_on(60)),
        (3, EventKind::Midi([0xe0, 0x00, 0x60])),
        (5, EventKind::Midi([0xb0, 1, 90])),
        (5, EventKind::Midi([0xd0, 70, 0])),
        (9, param(Macro::Algorithm, 12.0)),
        (9, param(Macro::Release, 0.7)),
        (11, note_off(60)),
        (40, EventKind::AllNotesOff),
    ];
    let out = play(&mut p, 512, 128, &events);
    assert!(out.l.iter().chain(&out.r).all(|x| x.is_finite()));
    reset(&mut p);
    // A one-frame call and a full-size call.
    let _ = play(&mut p, 1, 1, &[(0, note_on(40))]);
    let _ = play(
        &mut p,
        MAX_BLOCK,
        MAX_BLOCK,
        &[(MAX_BLOCK - 1, note_on(41))],
    );
}

#[test]
fn activate_checks_the_rate_and_rebuilds() {
    let mut p = activated(default_patch());
    let _ = play(&mut p, 512, 512, &[(0, note_on(60))]);
    assert_eq!(p.active_voices(), 1);
    assert!(matches!(
        p.activate(0.0, 512),
        Err(ProcessorError::UnsupportedSampleRate(_))
    ));
    assert!(p.activate(f32::NAN, 512).is_err());
    p.activate(44_100.0, 4096).unwrap();
    assert_eq!(
        (p.sample_rate(), p.active_voices(), p.latency()),
        (44_100.0, 0, 17)
    );
    assert_eq!(*p.patch(), default_patch());
    assert_eq!(PisynthProcessor::default().patch(), &default_patch());
}
