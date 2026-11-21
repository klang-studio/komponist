use std::ffi::c_char;
use std::sync::Arc;

use clap_sys::host::clap_host;
use clap_sys::version::CLAP_VERSION;
use nice_plug::prelude::*;

use super::*;

unsafe extern "C" fn no_host_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: c"SDK test host".as_ptr(),
    vendor: c"klang-studio".as_ptr(),
    url: c"https://github.com/klang-studio/komponist".as_ptr(),
    version: c"1".as_ptr(),
    get_extension: Some(no_host_extension),
    request_restart: None,
    request_process: None,
    request_callback: None,
};

struct Instance(*const clap_plugin);

impl Instance {
    fn new<P: ClapPlugin>() -> Self {
        // Keep the wrapper alive using the reference consumed by the plugin's destroy callback,
        // exactly as the nice-plug CLAP factory does.
        let wrapper = Arc::into_raw(unsafe { Wrapper::<P>::new(&HOST) });
        let plugin = unsafe { (*wrapper).clap_plugin.as_ptr() };
        let instance = Self(plugin);
        assert!(unsafe { ((*plugin).init.unwrap())(plugin) });
        instance
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe { ((*self.0).destroy.unwrap())(self.0) };
    }
}

#[derive(Default)]
struct TestPlugin<P: Params + Default>(Arc<P>);

impl<P: Params + Default> Plugin for TestPlugin<P> {
    const NAME: &'static str = "Layout test plugin";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = "0";
    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[];
    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.0.clone()
    }

    fn process(
        &mut self,
        _: &mut Buffer,
        _: &mut AuxiliaryBuffers,
        _: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        ProcessStatus::Normal
    }
}

impl<P: Params + Default> ClapPlugin for TestPlugin<P> {
    const CLAP_ID: &'static str = "org.klang-studio.komponist.sdk-test";
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[];
}

fn bilingual(english: &str, german: &str) -> Labels {
    [("en".into(), english.into()), ("de".into(), german.into())].into()
}

fn panel(cell: Cell) -> Panel {
    Panel {
        default_locale: "en".into(),
        locales: vec!["en".into(), "de".into()],
        grid: DeviceLayout::single(Page {
            name: "Main".into(),
            cols: 4,
            cells: vec![cell],
            dividers: Vec::new(),
        }),
        pages: vec![PageLabels {
            key: "main".into(),
            labels: bilingual("Main", "Hauptseite"),
        }],
        parameter_labels: BTreeMap::new(),
        choice_labels: BTreeMap::new(),
    }
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Clean,
    Drive,
}

#[derive(Params)]
struct ChoiceParams {
    #[id = "mode"]
    mode: EnumParam<Mode>,
    #[id = "count"]
    count: IntParam,
}

impl Default for ChoiceParams {
    fn default() -> Self {
        Self {
            mode: EnumParam::new("Mode", Mode::Drive),
            count: IntParam::new("Count", 0, IntRange::Linear { min: 0, max: 1 }),
        }
    }
}

impl NativeClapLayout for TestPlugin<ChoiceParams> {
    fn panel() -> Panel {
        let id = ParamId::new("mode");
        let mut panel = panel(Cell::half(
            0,
            0,
            2,
            Half::Top,
            WidgetKind::Segment,
            CellParam::Id(id.clone()),
        ));
        panel.choice_labels.insert(
            id,
            vec![bilingual("Clean", "Klar"), bilingual("Drive", "Verzerrt")],
        );
        panel
    }
}

#[test]
fn live_enum_ids_and_translations_survive_two_identical_abi_copies() {
    type P = TestPlugin<ChoiceParams>;
    let instance = Instance::new::<P>();
    let plugin = instance.0;
    let params = unsafe { inventory(plugin).unwrap() };
    let mode = unsafe { Wrapper::<P>::custom_extension_param_id(plugin, "mode").unwrap() };
    let count = unsafe { Wrapper::<P>::custom_extension_param_id(plugin, "count").unwrap() };
    let info = |id| params.iter().find(|param| param.id == id).unwrap();
    assert_eq!(info(mode).default, 1.0);
    assert_ne!(info(mode).flags & param_flags::ENUM, 0);
    assert_ne!(info(count).flags & param_flags::STEPPED, 0);
    assert_eq!(info(count).flags & param_flags::ENUM, 0);

    assert!(extension::<P>(c"future.extension/2").is_null());
    let table = unsafe { &*extension::<P>(EXTENSION_ID).cast::<LayoutExtension>() };
    let mut size = 0;
    assert_eq!(
        unsafe { (table.get_layout)(plugin, ptr::null_mut(), 0, &mut size) },
        BUFFER_TOO_SMALL
    );
    let mut first = vec![0; size as usize];
    let mut second = vec![0; size as usize];
    for buffer in [&mut first, &mut second] {
        let mut written = 0;
        assert_eq!(
            unsafe { (table.get_layout)(plugin, buffer.as_mut_ptr(), size, &mut written) },
            OK
        );
        assert_eq!(written, size);
    }
    assert_eq!(first, second);
    let parsed = NativeLayout::parse(&first).unwrap();
    parsed.validate_params(&params).unwrap();
    let cell = &parsed.pages[0].cells[0];
    assert_eq!(cell.param_id, mode);
    let choices = cell.choices.as_ref().unwrap();
    assert_eq!(choices[1].value, 1);
    assert_eq!(parsed.label(&choices[1].labels, "de-DE"), Some("Verzerrt"));
    assert_eq!(parsed.label(&choices[1].labels, "fr"), Some("Drive"));

    let mut integer_segment = P::panel();
    integer_segment.grid.pages[0].cells[0].param = CellParam::Id(ParamId::new("count"));
    let mode_labels = integer_segment
        .choice_labels
        .remove(&ParamId::new("mode"))
        .unwrap();
    integer_segment
        .choice_labels
        .insert(ParamId::new("count"), mode_labels);
    assert!(unsafe { compile::<P>(plugin, &integer_segment) }.is_err());
}

#[derive(Params)]
struct CollisionParams {
    #[id = "Aa"]
    first: FloatParam,
    #[id = "BB"]
    second: FloatParam,
}

impl Default for CollisionParams {
    fn default() -> Self {
        let range = FloatRange::Linear { min: 0.0, max: 1.0 };
        Self {
            first: FloatParam::new("First", 0.5, range),
            second: FloatParam::new("Second", 0.5, range),
        }
    }
}

#[test]
fn layout_compilation_rejects_a_live_wrapper_id_collision() {
    type P = TestPlugin<CollisionParams>;
    let instance = Instance::new::<P>();
    let panel = panel(layout::knob(0, 0, "Aa"));
    assert_eq!(
        unsafe { Wrapper::<P>::custom_extension_param_id(instance.0, "Aa") },
        None
    );
    assert!(unsafe { compile::<P>(instance.0, &panel) }.is_err());
}
