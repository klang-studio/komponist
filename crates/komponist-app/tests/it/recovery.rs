//! #227 crash recovery through the app (crash-recovery proposal, internal notes, §7): the copy
//! follows the unsaved changes on a fake clock and goes when they are saved, undone or discarded; a
//! session that ends without quitting leaves it for the next start; the prompt (variants a–e, Enter,
//! Esc, the counter, de and en snapshots); recovering opens dirty with the original path and Ctrl S
//! writes the original file; the start screen's chip and own row; a panic on the UI thread writes
//! the copy (a child process).
//!
//! Every test has its own scratch folder under `CARGO_TARGET_TMPDIR` for the recovery folder, the
//! settings and the projects; never the user's folders. The engine runs on the null backend.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::device_host::BuiltinHost;
use komponist_app::dialogs::recovery::{self as prompt, Answer, Model, Variant};
use komponist_app::dialogs::start;
use komponist_app::engine_sync::EngineSync;
use komponist_app::project_io;
use komponist_app::recovery::{self, EndedBy, Id, RecoveryMeta};
use komponist_app::settings::Settings;
use komponist_app::ui_state::{Screen, StartView};
use komponist_app::{Action, App, EditAction, ProjectAction};
use komponist_audio::HostKind;
use komponist_i18n::{Lang, de, en};
use komponist_model::fixtures::demo_project;
use komponist_model::{Document, Project};

const W: f32 = 1920.0;
const H: f32 = 1080.0;
const WAIT: Duration = Duration::from_secs(10);

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("komponist-recovery-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A clock the test moves.
#[derive(Clone)]
struct FakeClock(Arc<Mutex<Instant>>);

impl FakeClock {
    fn new() -> FakeClock {
        FakeClock(Arc::new(Mutex::new(Instant::now())))
    }

    fn clock(&self) -> recovery::Clock {
        let now = Arc::clone(&self.0);
        Arc::new(move || *now.lock().unwrap())
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

/// The app of one session: settings (and the recent list) and the recovery folder in `dir`.
fn session(dir: &Path, project: Project, clock: &FakeClock) -> App {
    let mut settings = Settings::default();
    settings.audio.host = HostKind::Null;
    let engine = EngineSync::null(Box::new(BuiltinHost::new()));
    App::new(
        Document::new(project),
        engine,
        settings,
        Some(dir.join("config/settings.json")),
    )
    .with_recovery_clock(dir.join("recovery"), clock.clock())
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(W, H))
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(2);
    h
}

fn press(h: &mut Harness<'_, App>, mods: Modifiers, key: Key) {
    h.key_press_modifiers(mods, key);
    h.run_steps(2);
}

fn edit(h: &mut Harness<'_, App>, action: EditAction) {
    h.state_mut().dispatch(Action::Edit(action));
    h.run_steps(2);
}

/// Lets the clock pass `by`, runs a frame and waits for the writer.
fn wait(h: &mut Harness<'_, App>, clock: &FakeClock, by: Duration) {
    clock.advance(by);
    h.run_steps(1);
    assert!(h.state().flush_recovery(WAIT));
}

/// The copies (`*.komponist`) in the recovery folder of `dir`.
fn copies(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir.join("recovery"))
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "komponist"))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

fn meta_of(copy: &Path) -> RecoveryMeta {
    let text = std::fs::read_to_string(copy.with_extension("json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn type_into(h: &mut Harness<'_, App>, label: &str, text: &str) {
    h.get_by_label(label).focus();
    h.run_steps(1);
    press(h, Modifiers::COMMAND, Key::A);
    h.get_by_label(label).type_text(text);
    h.run_steps(1);
}

/// A session that edits `file` (or an untitled project) and ends without quitting, as a crash
/// would: its copy stays.
fn crashed_session(dir: &Path, file: Option<&Path>) {
    let clock = FakeClock::new();
    let mut app = session(dir, Project::new("Unbenannt"), &clock);
    if let Some(file) = file {
        app.dispatch(Action::Project(ProjectAction::OpenPath(file.to_owned())));
    }
    let mut h = harness(app);
    edit(&mut h, EditAction::NewMidiTrack);
    wait(&mut h, &clock, recovery::QUIET);
    // Dropped without `shutdown`: nothing deletes the copy.
}

/// A saved project without foreign plugins (the demo's would open S06 on top).
fn song(name: &str) -> Project {
    Project::new(name)
}

// ------------------------------------------------------------------------------------------------
// The copy follows the unsaved changes
// ------------------------------------------------------------------------------------------------

#[test]
fn the_copy_follows_unsaved_changes_and_goes_when_they_are_saved_undone_or_discarded() {
    let dir = scratch("follow");
    let clock = FakeClock::new();
    let mut h = harness(session(&dir, demo_project(), &clock));
    // Clean: no copy.
    wait(&mut h, &clock, Duration::from_secs(60));
    assert!(copies(&dir).is_empty());
    // An edit: the copy 2 s after it, not before.
    edit(&mut h, EditAction::NewMidiTrack);
    wait(&mut h, &clock, Duration::from_millis(1500));
    assert!(copies(&dir).is_empty(), "not before 2 s");
    wait(&mut h, &clock, Duration::from_millis(500));
    let copy = copies(&dir).pop().expect("a copy 2 s after the change");
    let saved = project_io::load_project(&copy).unwrap();
    assert_eq!(saved, **h.state().project(), "the copy is the document");
    let meta = meta_of(&copy);
    assert_eq!(meta.project_path, None);
    assert_eq!(meta.name, demo_project().name);
    assert_eq!(meta.ended_by, EndedBy::Unknown);

    // Saved (Ctrl S into a new file): the copy goes.
    press(&mut h, Modifiers::COMMAND, Key::S);
    type_into(
        &mut h,
        de::FILE_NAME,
        dir.join("Gespeichert").to_str().unwrap(),
    );
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(!h.state().doc().is_dirty());
    wait(&mut h, &clock, Duration::ZERO);
    assert!(copies(&dir).is_empty(), "saved: no copy");

    // Undone back to the saved state: the copy goes.
    edit(&mut h, EditAction::NewMidiTrack);
    wait(&mut h, &clock, recovery::QUIET);
    let copy = copies(&dir)
        .pop()
        .expect("a copy of the saved project's changes");
    assert_eq!(
        meta_of(&copy).project_path.as_deref(),
        Some(dir.join("Gespeichert.komponist").as_path())
    );
    edit(&mut h, EditAction::Undo);
    wait(&mut h, &clock, Duration::ZERO);
    assert!(
        copies(&dir).is_empty(),
        "undone to the saved state: no copy"
    );

    // „Nicht speichern“ on Ctrl N: the copy goes, and so does its lock.
    edit(&mut h, EditAction::NewMidiTrack);
    wait(&mut h, &clock, recovery::QUIET);
    assert_eq!(copies(&dir).len(), 1);
    press(&mut h, Modifiers::COMMAND, Key::N);
    h.get_by_label(de::NEW_PROJECT_DISCARD).click();
    h.run_steps(2);
    wait(&mut h, &clock, Duration::ZERO);
    assert!(copies(&dir).is_empty(), "discarded: no copy");
    let locks = std::fs::read_dir(dir.join("recovery"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "lock"))
        .count();
    assert_eq!(locks, 0, "the closed document's lock is gone");

    // Quitting normally removes a copy too.
    edit(&mut h, EditAction::NewMidiTrack);
    wait(&mut h, &clock, recovery::QUIET);
    assert_eq!(copies(&dir).len(), 1);
    h.state_mut().shutdown();
    assert!(copies(&dir).is_empty(), "quit: no copy");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_live_session_hides_its_copy_from_a_second_one() {
    let dir = scratch("live");
    let clock = FakeClock::new();
    let mut h = harness(session(&dir, demo_project(), &clock));
    edit(&mut h, EditAction::NewMidiTrack);
    wait(&mut h, &clock, recovery::QUIET);
    assert_eq!(copies(&dir).len(), 1);
    // A second komponist at the same time does not offer it.
    let mut second = session(&dir, Project::new("Unbenannt"), &clock);
    second.show_start_screen();
    second.offer_recovery(None);
    assert!(!second.recovery_prompt_open());
    // After the first one ends without quitting, the next start does.
    drop(h);
    let mut third = session(&dir, Project::new("Unbenannt"), &clock);
    third.show_start_screen();
    third.offer_recovery(None);
    assert!(third.recovery_prompt_open());
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// Recovering
// ------------------------------------------------------------------------------------------------

#[test]
fn recover_opens_the_copy_dirty_under_its_file_and_ctrl_s_writes_that_file() {
    let dir = scratch("recover");
    let songs = dir.join("songs");
    std::fs::create_dir_all(&songs).unwrap();
    let file = songs.join("Night Drive.komponist");
    project_io::save_project(&file, &song("Night Drive")).unwrap();
    crashed_session(&dir, Some(&file));
    let copy = copies(&dir).pop().expect("the crashed session's copy");

    // The next start asks before the start screen: variant a.
    let clock = FakeClock::new();
    let mut app = session(&dir, Project::new("Unbenannt"), &clock);
    app.show_start_screen();
    app.offer_recovery(None);
    let mut h = harness(app);
    h.get_by_role_and_label(Role::Dialog, de::RECOVERY_TITLE);
    h.get_by_label(&Lang::De.recovery_text_saved("Night Drive"));
    h.get_by_role_and_label(Role::Button, de::RECOVERY_RECOVER);
    h.get_by_role_and_label(Role::Button, de::RECOVERY_LATER);
    h.get_by_role_and_label(Role::Button, de::RECOVERY_DISCARD);
    assert!(h.query_by_label(&Lang::De.recovery_counter(1, 1)).is_none());
    // Enter recovers.
    press(&mut h, Modifiers::NONE, Key::Enter);
    let app = h.state();
    assert_eq!(app.ui().screen, Screen::Project);
    assert_eq!(app.project().name, "Night Drive");
    assert_eq!(app.project().tracks.len(), 1);
    assert!(app.doc().is_dirty(), "recovered changes are unsaved");
    assert_eq!(app.project_path(), Some(file.as_path()));
    let status = app.ui().status.as_ref().unwrap();
    assert!(
        status.text.starts_with("Änderungen von ")
            && status
                .text
                .ends_with("speichert sie in „Night Drive.komponist“"),
        "{}",
        status.text
    );
    // The project's accessible name says it is unsaved.
    h.get_by_label(&Lang::De.transport_project_unsaved("Night Drive"));
    // The copy stays until the first save.
    wait(&mut h, &clock, recovery::QUIET);
    assert_eq!(copies(&dir), [copy.clone()], "the same entry, kept");
    // Ctrl S writes the original file (no file browser) and removes the copy.
    press(&mut h, Modifiers::COMMAND, Key::S);
    assert!(h.query_by_value(de::FILE_SAVE_TITLE).is_none());
    assert!(!h.state().doc().is_dirty());
    assert_eq!(project_io::load_project(&file).unwrap().tracks.len(), 1);
    wait(&mut h, &clock, Duration::ZERO);
    assert!(copies(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_copy_whose_file_is_gone_asks_where_to_save() {
    let dir = scratch("gone");
    let songs = dir.join("songs");
    std::fs::create_dir_all(&songs).unwrap();
    let file = songs.join("Night Drive.komponist");
    project_io::save_project(&file, &song("Night Drive")).unwrap();
    crashed_session(&dir, Some(&file));
    std::fs::remove_file(&file).unwrap();

    let clock = FakeClock::new();
    let mut app = session(&dir, Project::new("Unbenannt"), &clock);
    app.show_start_screen();
    app.offer_recovery(None);
    let mut h = harness(app);
    // Variant d: the file fact and the notice say so.
    let path = komponist_app::dialogs::format::tilde(&file);
    h.get_by_label(&Lang::De.recovery_file_gone(&path));
    h.get_by_label(de::RECOVERY_NOTICE_GONE);
    h.get_by_role_and_label(Role::Button, de::RECOVERY_RECOVER)
        .click();
    h.run_steps(2);
    assert_eq!(h.state().project().name, "Night Drive");
    assert!(h.state().doc().is_dirty());
    // Ctrl S asks for a place, in the old folder.
    press(&mut h, Modifiers::COMMAND, Key::S);
    h.run_steps(1);
    h.get_by_value(de::FILE_SAVE_TITLE);
    // The browser shows the old folder: a bare name saves there.
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(
        file.exists(),
        "saved into the old folder under the old name"
    );
    assert!(!h.state().doc().is_dirty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn later_and_discard_go_through_the_copies_and_then_open_the_project_argument() {
    let dir = scratch("round");
    let songs = dir.join("songs");
    std::fs::create_dir_all(&songs).unwrap();
    let night = songs.join("Night Drive.komponist");
    project_io::save_project(&night, &song("Night Drive")).unwrap();
    let other = songs.join("Hafenlicht.komponist");
    project_io::save_project(&other, &Project::new("Hafenlicht")).unwrap();
    crashed_session(&dir, None);
    std::thread::sleep(Duration::from_millis(5));
    crashed_session(&dir, Some(&night));
    assert_eq!(copies(&dir).len(), 2);

    let clock = FakeClock::new();
    let mut app = session(&dir, Project::new("Unbenannt"), &clock);
    app.show_start_screen();
    app.offer_recovery(Some(other.clone()));
    let mut h = harness(app);
    // Newest first, counted in the header.
    h.get_by_label(&Lang::De.recovery_text_saved("Night Drive"));
    h.get_by_label(&Lang::De.recovery_counter(1, 2));
    // Esc is „Später“: the next copy.
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.get_by_label(&Lang::De.recovery_text_untitled("Unbenannt"));
    h.get_by_label(&Lang::De.recovery_counter(2, 2));
    h.get_by_label(de::RECOVERY_FILE_NEVER);
    // „Verwerfen“ moves it to `discarded/`; then the project argument opens.
    h.get_by_role_and_label(Role::Button, de::RECOVERY_DISCARD)
        .click();
    h.run_steps(2);
    assert!(!h.state().recovery_prompt_open());
    assert_eq!(h.state().project().name, "Hafenlicht");
    assert_eq!(copies(&dir).len(), 1, "the later one stays");
    let discarded = std::fs::read_dir(dir.join("recovery").join(recovery::DISCARDED))
        .unwrap()
        .count();
    assert_eq!(discarded, 2, "copy and meta kept for 7 days");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_start_screen_marks_rows_with_copies_and_opening_one_asks_first() {
    let dir = scratch("start");
    let songs = dir.join("songs");
    std::fs::create_dir_all(&songs).unwrap();
    let night = songs.join("Night Drive.komponist");
    project_io::save_project(&night, &song("Night Drive")).unwrap();
    crashed_session(&dir, None);
    crashed_session(&dir, Some(&night));

    let clock = FakeClock::new();
    let mut app = session(&dir, Project::new("Unbenannt"), &clock);
    app.show_start_screen();
    app.offer_recovery(None);
    let mut h = harness(app);
    // „Später“ for both.
    press(&mut h, Modifiers::NONE, Key::Escape);
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(!h.state().recovery_prompt_open());
    h.run_steps(2);
    // The untitled copy has its own row at the top; the recent row of Night Drive has the chip.
    let untitled = h.get_all_by_role(Role::ListItem).next().unwrap();
    let name = untitled.accesskit_node().label().unwrap_or_default();
    assert!(
        name.starts_with("Unbenannt, ") && name.ends_with(", Sicherungskopie vorhanden"),
        "{name}"
    );
    let night_row = h
        .get_all_by_role(Role::ListItem)
        .find(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with("Night Drive, "))
        })
        .unwrap();
    assert!(
        night_row
            .accesskit_node()
            .label()
            .unwrap()
            .ends_with(", Sicherungskopie vorhanden")
    );
    // Opening that row asks for its copy first; „Später“ then opens the file.
    night_row.click();
    h.run_steps(2);
    h.get_by_label(&Lang::De.recovery_text_saved("Night Drive"));
    h.get_by_role_and_label(Role::Button, de::RECOVERY_LATER)
        .click();
    h.run_steps(2);
    assert_eq!(h.state().ui().screen, Screen::Project);
    assert_eq!(h.state().project().name, "Night Drive");
    assert!(!h.state().doc().is_dirty(), "the file, not the copy");
    assert_eq!(copies(&dir).len(), 2, "both copies wait for the next start");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_take_a_copy_refers_to_survives_saves() {
    let dir = scratch("takes");
    // A take in a provisional recordings folder of a crashed session.
    let take = dir.join("Aufnahmen/Unbenannt-2026-11-23/Take 1.wav");
    std::fs::create_dir_all(take.parent().unwrap()).unwrap();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&take, spec).unwrap();
    for i in 0..4_800 {
        writer.write_sample((i % 100) as i16).unwrap();
    }
    writer.finalize().unwrap();
    let (project, track) = komponist_model::edits::add_audio_track(
        &Project::new("Unbenannt"),
        komponist_model::edits::NewAudioTrack::default(),
    )
    .unwrap();
    let (project, _) = komponist_model::edits::add_clip(
        &project,
        track,
        komponist_model::edits::NewClip {
            start: komponist_model::Tick::ZERO,
            length: komponist_model::Tick(960),
            name: "Take 1".into(),
            content: komponist_model::edits::NewClipContent::Audio {
                file: komponist_model::AudioFile::new(&take, None, 4_800, 48_000, 1),
                offset: 0,
            },
        },
    )
    .unwrap();
    {
        // The take's project, unsaved (one more track), in a session that crashes.
        let clock = FakeClock::new();
        let mut h = harness(session(&dir, project, &clock));
        edit(&mut h, EditAction::NewMidiTrack);
        wait(&mut h, &clock, recovery::QUIET);
    }
    assert_eq!(copies(&dir).len(), 1);

    // The next session says „Später“, works on another project and saves it: the take stays.
    let clock = FakeClock::new();
    let mut app = session(&dir, Project::new("Unbenannt"), &clock);
    app.show_start_screen();
    app.offer_recovery(None);
    let mut h = harness(app);
    press(&mut h, Modifiers::NONE, Key::Escape);
    press(&mut h, Modifiers::COMMAND, Key::N);
    edit(&mut h, EditAction::NewMidiTrack);
    press(&mut h, Modifiers::COMMAND, Key::S);
    type_into(&mut h, de::FILE_NAME, dir.join("Anderes").to_str().unwrap());
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(dir.join("Anderes.komponist").exists());
    assert!(take.exists(), "a save of another project keeps the take");
    // Recovering and saving the copy keeps it too.
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::ShowStartScreen));
    h.run_steps(2);
    let row = h
        .get_all_by_role(Role::ListItem)
        .find(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l.ends_with(", Sicherungskopie vorhanden"))
        })
        .unwrap();
    row.click();
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(
        h.state().project().tracks.len(),
        2,
        "the audio track and the new one"
    );
    press(&mut h, Modifiers::COMMAND, Key::S);
    h.run_steps(1);
    type_into(
        &mut h,
        de::FILE_NAME,
        dir.join("Mit Take").to_str().unwrap(),
    );
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(dir.join("Mit Take.komponist").exists());
    assert!(take.exists(), "saving the recovered project keeps the take");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// The panic hook
// ------------------------------------------------------------------------------------------------

/// Set for the child process of [`a_panic_on_the_ui_thread_writes_the_copy`]: its scratch folder.
const PANIC_CHILD: &str = "KOMPONIST_RECOVERY_PANIC_CHILD";

/// The child: an edit, one frame (the project is published, no copy written yet), then a panic on
/// this, the UI thread. Without the variable it does nothing.
#[test]
fn panic_child() {
    let Some(dir) = std::env::var_os(PANIC_CHILD).map(PathBuf::from) else {
        return;
    };
    let clock = FakeClock::new();
    let mut app = session(&dir, demo_project(), &clock);
    recovery::install_panic_hook(app.recovery_panic_handle().unwrap());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let mut h = harness(app);
    h.run_steps(1);
    assert!(copies(&dir).is_empty(), "nothing written before the panic");
    panic!("recovery test: the UI thread panics");
}

#[test]
fn a_panic_on_the_ui_thread_writes_the_copy() {
    let dir = scratch("panic");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "recovery::panic_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PANIC_CHILD, &dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "the child panicked");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("recovery test: the UI thread panics"),
        "the previous hook still prints: {stderr}"
    );
    let copy = copies(&dir).pop().expect("the hook wrote the copy");
    let project = project_io::load_project(&copy).unwrap();
    assert_eq!(project.tracks.len(), demo_project().tracks.len() + 1);
    let EndedBy::Panic { message, location } = meta_of(&copy).ended_by else {
        panic!("ended_by is panic");
    };
    assert_eq!(message, "recovery test: the UI thread panics");
    assert!(location.contains("recovery.rs"), "{location}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------
// The prompt's view: variants, keys, snapshots
// ------------------------------------------------------------------------------------------------

fn model(t: Lang, variant: Variant) -> Model {
    let (copy, file) = match t {
        Lang::De => ("heute 14:32", "heute 13:05"),
        _ => ("today 14:32", "today 13:05"),
    };
    let untitled = matches!(variant, Variant::Untitled);
    Model {
        name: if untitled {
            t.new_project_default_name().into()
        } else {
            "Night Drive".into()
        },
        copy_time: copy.into(),
        file_time: (!untitled).then(|| {
            if matches!(variant, Variant::FileNewer) {
                file.replace("13:05", "14:40")
            } else {
                file.into()
            }
        }),
        path: (!untitled).then(|| "~/Musik/klang/Night Drive.komponist".into()),
        counter: None,
        variant,
    }
}

fn prompt_harness(lang: Lang, model: Model, ppp: f32) -> Harness<'static, Option<Answer>> {
    let mut h = komponist_ui::snapshot::builder::<Option<Answer>>(ppp)
        .with_size(vec2(760.0, 460.0))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .build_ui_state(
            move |ui, answer: &mut Option<Answer>| {
                komponist_ui::set_lang(ui.ctx(), lang);
                if let Some(a) = prompt::show(ui.ctx(), &model) {
                    *answer = Some(a);
                }
            },
            None,
        );
    h.run_steps(3);
    h
}

fn variants(t: Lang) -> Vec<(&'static str, Model)> {
    vec![
        ("a", model(t, Variant::Saved)),
        ("b", model(t, Variant::Untitled)),
        ("c", model(t, Variant::FileNewer)),
        ("d", model(t, Variant::FileGone)),
        (
            "e",
            model(
                t,
                Variant::Unreadable {
                    reason: t.project_reason_too_new(7),
                    untitled: false,
                },
            ),
        ),
        (
            "counter",
            Model {
                counter: Some((1, 2)),
                name: "Hafenlicht".into(),
                path: Some("~/Musik/klang/Hafenlicht.komponist".into()),
                ..model(t, Variant::Saved)
            },
        ),
    ]
}

#[test]
fn snapshots_of_the_prompt_variants_in_german_and_english() {
    let mut results = SnapshotResults::new();
    for (lang, suffix) in [(Lang::De, ""), (Lang::En, "-en")] {
        for (name, model) in variants(lang) {
            let mut h = prompt_harness(lang, model, 1.0);
            h.snapshot(format!("recovery-prompt-{name}{suffix}-1x"));
            results.extend_harness(&mut h);
        }
    }
    // The first case at 2×, too.
    let mut h = prompt_harness(Lang::De, model(Lang::De, Variant::Saved), 2.0);
    h.snapshot("recovery-prompt-a-2x");
    results.extend_harness(&mut h);
    results.unwrap();
}

#[test]
fn the_prompt_is_a_dialog_with_its_buttons_keys_and_counter() {
    for lang in [Lang::De, Lang::En] {
        let t = lang;
        // Role Dialog named by the title; the counter is a label in the header too.
        let mut model_counter = model(t, Variant::Saved);
        model_counter.counter = Some((1, 2));
        let h = prompt_harness(lang, model_counter, 1.0);
        h.get_by_role_and_label(Role::Dialog, t.recovery_title());
        h.get_by_label(&t.recovery_counter(1, 2));
        h.get_by_role_and_label(Role::Button, t.dialog_close());
        for button in [
            t.recovery_discard(),
            t.recovery_later(),
            t.recovery_recover(),
        ] {
            h.get_by_role_and_label(Role::Button, button);
        }
        // Enter recovers, Esc postpones, × postpones.
        let mut h = prompt_harness(lang, model(t, Variant::Saved), 1.0);
        h.key_press(Key::Enter);
        h.run_steps(1);
        assert_eq!(*h.state(), Some(Answer::Recover));
        let mut h = prompt_harness(lang, model(t, Variant::Saved), 1.0);
        h.key_press(Key::Escape);
        h.run_steps(1);
        assert_eq!(*h.state(), Some(Answer::Later));
        let mut h = prompt_harness(lang, model(t, Variant::Saved), 1.0);
        h.get_by_role_and_label(Role::Button, t.dialog_close())
            .click();
        h.run_steps(1);
        assert_eq!(*h.state(), Some(Answer::Later));
        // An unreadable copy has no „Wiederherstellen“, and Enter is „Später“.
        let e = model(
            t,
            Variant::Unreadable {
                reason: t.project_reason_damaged().into(),
                untitled: false,
            },
        );
        let mut h = prompt_harness(lang, e, 1.0);
        assert!(
            h.query_by_role_and_label(Role::Button, t.recovery_recover())
                .is_none()
        );
        h.key_press(Key::Enter);
        h.run_steps(1);
        assert_eq!(*h.state(), Some(Answer::Later));
        // „Verwerfen“.
        let mut h = prompt_harness(lang, model(t, Variant::Untitled), 1.0);
        h.get_by_role_and_label(Role::Button, t.recovery_discard())
            .click();
        h.run_steps(1);
        assert_eq!(*h.state(), Some(Answer::Discard));
    }
    assert_eq!(en::RECOVERY_TITLE, "Recover Unsaved Changes?");
    assert_eq!(
        de::RECOVERY_TITLE,
        "Ungespeicherte Änderungen wiederherstellen?"
    );
}

/// Nothing of the English prompt is cut: every text fits the dialog (the snapshots show it; this
/// checks the widest case's labels are whole).
#[test]
fn the_english_prompt_texts_are_whole() {
    let t = Lang::En;
    let h = prompt_harness(t, model(t, Variant::FileNewer), 1.0);
    h.get_by_label(&t.recovery_text_saved("Night Drive"));
    h.get_by_label(t.recovery_notice_newer());
    h.get_by_label(&t.recovery_file_saved("today 14:40", "~/Musik/klang/Night Drive.komponist"));
    h.get_by_label(t.recovery_fact_copy());
    h.get_by_label(t.recovery_fact_file());
}

// ------------------------------------------------------------------------------------------------
// The start screen's chip and own row
// ------------------------------------------------------------------------------------------------

fn start_model(t: Lang) -> start::Model {
    let id = |s: &str| Id::parse(s);
    let row = |name: &str, path: &str, meta: &str, date: &str, hues: &[u16], copy: Option<Id>| {
        start::RecentRow {
            path: PathBuf::from(path),
            name: name.to_owned(),
            path_label: if path.is_empty() {
                t.recovery_file_never().to_owned()
            } else {
                path.to_owned()
            },
            meta: meta.to_owned(),
            date: date.to_owned(),
            hues: hues.to_vec(),
            missing: false,
            recovery: copy,
        }
    };
    let (tracks, yesterday, hours, oct) = match t {
        Lang::De => ("Spuren", "gestern", "vor 2 Stunden", "12. Okt."),
        _ => ("tracks", "yesterday", "2 hours ago", "Oct 12"),
    };
    start::Model {
        version: start::version_line_for(t, komponist_app::platform::Platform::Pc),
        new_shortcut: if t == Lang::De { "Strg N" } else { "Ctrl+N" }.into(),
        open_shortcut: if t == Lang::De { "Strg O" } else { "Ctrl+O" }.into(),
        recent: vec![
            row(
                t.new_project_default_name(),
                "",
                &format!("120 BPM · 2 {tracks}"),
                yesterday,
                &[30, 250],
                id("00000000000000a1"),
            ),
            row(
                "Night Drive",
                "~/Musik/klang/Night Drive.komponist",
                &format!("124 BPM · 6 {tracks}"),
                hours,
                &[30, 250, 300, 140, 60, 190],
                id("00000000000000b2"),
            ),
            row(
                "Hafenlicht",
                "~/Musik/klang/Hafenlicht.komponist",
                &format!("98 BPM · 9 {tracks}"),
                yesterday,
                &[30, 250, 300, 140, 60, 190, 10, 100, 20],
                None,
            ),
            row(
                "Beat-Skizze 14",
                "~/Musik/klang/skizzen/beat-14.komponist",
                &format!("140 BPM · 3 {tracks}"),
                oct,
                &[30, 250, 300],
                None,
            ),
        ],
        system: Vec::new(),
        message: None,
    }
}

#[test]
fn snapshots_of_the_start_screen_with_recovery_copies() {
    let mut results = SnapshotResults::new();
    for (lang, name) in [
        (Lang::De, "recovery-start-1x"),
        (Lang::En, "recovery-start-en-1x"),
    ] {
        let model = start_model(lang);
        let mut view = StartView::default();
        // The second row (Night Drive) selected, as in mockup 03.
        view.selected = 1;
        let mut h = komponist_ui::snapshot::builder::<(StartView, Vec<start::Output>)>(1.0)
            .with_size(vec2(W, H))
            .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
            .build_ui_state(
                move |ui, (view, out): &mut (StartView, Vec<start::Output>)| {
                    komponist_ui::set_lang(ui.ctx(), lang);
                    let rect = ui.ctx().content_rect();
                    out.extend(start::show(ui, rect, &model, view, true));
                },
                (view, Vec::new()),
            );
        h.run_steps(3);
        // Accessible names carry the chip; the untitled row opens its copy.
        let t = lang;
        let first = h.get_all_by_role(Role::ListItem).next().unwrap();
        assert_eq!(
            first.accesskit_node().label().as_deref(),
            Some(
                t.start_recovery_row(format!(
                    "{}, {}",
                    t.new_project_default_name(),
                    start_model(t).recent[0].meta
                ))
                .as_str()
            )
        );
        h.snapshot(name);
        results.extend_harness(&mut h);
        first_row_opens_its_copy(&mut h);
    }
    results.unwrap();
}

fn first_row_opens_its_copy(h: &mut Harness<'_, (StartView, Vec<start::Output>)>) {
    h.get_all_by_role(Role::ListItem).next().unwrap().click();
    h.run_steps(1);
    assert_eq!(
        h.state().1.last(),
        Some(&start::Output::Recovery(
            Id::parse("00000000000000a1").unwrap()
        ))
    );
}
