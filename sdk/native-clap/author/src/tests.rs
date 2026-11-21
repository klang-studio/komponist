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
        let wrapper = Arc::into_raw(unsafe { Wrapper::<P>::new(&HOST).unwrap() });
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
    Warm,
    Bright,
    Dark,
    Soft,
    Hard,
}

#[derive(Params)]
struct ChoiceParams {
    #[id = "mode"]
    mode: EnumParam<Mode>,
    #[id = "count"]
    count: IntParam,
    #[id = "plain"]
    plain: FloatParam,
}

impl Default for ChoiceParams {
    fn default() -> Self {
        Self {
            mode: EnumParam::new("Mode", Mode::Soft),
            count: IntParam::new("Count", 0, IntRange::Linear { min: 0, max: 1 }),
            plain: FloatParam::new("Plain", 0.375, FloatRange::Linear { min: 0.0, max: 1.0 }),
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
            vec![
                bilingual("Clean", "Klar"),
                bilingual("Drive", "Verzerrt"),
                bilingual("Warm", "Warm"),
                bilingual("Bright", "Hell"),
                bilingual("Dark", "Dunkel"),
                bilingual("Soft", "Weich"),
                bilingual("Hard", "Hart"),
            ],
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
    assert_eq!(info(mode).default, 5.0);
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

struct DuplicateParams {
    first: FloatParam,
    second: FloatParam,
}

// SAFETY: Both pointers refer to parameters owned by the Params Arc. A hand-written map is needed
// to test duplicate IDs because the derive checks duplicate field IDs at compile time.
unsafe impl Params for DuplicateParams {
    fn param_map(&self) -> Vec<(String, ParamPtr, String)> {
        vec![
            ("same".into(), self.first.as_ptr(), String::new()),
            ("same".into(), self.second.as_ptr(), String::new()),
        ]
    }
}

impl Default for DuplicateParams {
    fn default() -> Self {
        let range = FloatRange::Linear { min: 0.0, max: 1.0 };
        Self {
            first: FloatParam::new("First", 0.5, range),
            second: FloatParam::new("Second", 0.5, range),
        }
    }
}

#[test]
fn wrapper_rejects_colliding_hashes_and_duplicate_author_ids_before_publication() {
    assert!(unsafe { Wrapper::<TestPlugin<CollisionParams>>::new(&HOST) }.is_none());
    assert!(unsafe { Wrapper::<TestPlugin<DuplicateParams>>::new(&HOST) }.is_none());
}

// Both test plugin types publish the same descriptor ID.
nice_export_clap!(TestPlugin<ChoiceParams>, TestPlugin<CollisionParams>);

#[test]
fn duplicate_descriptor_ids_disable_the_factory() {
    use clap_sys::factory::plugin_factory::CLAP_PLUGIN_FACTORY_ID;

    assert!(!clap::init(c"duplicate.clap".as_ptr()));
    assert!(clap::get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()).is_null());
}

fn clap_params(instance: &Instance) -> &clap_plugin_params {
    let plugin = instance.0;
    let table = unsafe { ((*plugin).get_extension.unwrap())(plugin, CLAP_EXT_PARAMS.as_ptr()) };
    assert!(!table.is_null());
    unsafe { &*table.cast::<clap_plugin_params>() }
}

#[test]
fn stepped_values_roundtrip_exactly_and_the_inventory_rejects_its_end_index() {
    type P = TestPlugin<ChoiceParams>;
    let instance = Instance::new::<P>();
    let plugin = instance.0;
    let table = clap_params(&instance);
    let id =
        |author: &str| unsafe { Wrapper::<P>::custom_extension_param_id(plugin, author) }.unwrap();
    let mode = id("mode");
    let plain = id("plain");
    let mut value = 0.0;
    assert!(unsafe { table.get_value.unwrap()(plugin, mode, &mut value) });
    assert_eq!(value, 5.0);
    assert!(unsafe { table.text_to_value.unwrap()(plugin, mode, c"Soft".as_ptr(), &mut value) });
    assert_eq!(value, 5.0);
    assert!(unsafe { table.get_value.unwrap()(plugin, plain, &mut value) });
    assert_eq!(value, 0.375);
    let count = unsafe { table.count.unwrap()(plugin) };
    let mut info = MaybeUninit::uninit();
    assert!(!unsafe { table.get_info.unwrap()(plugin, count, info.as_mut_ptr()) });
}

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
enum BadLabels {
    Valid,
    #[name = "   "]
    Blank,
    #[name = "\0Invisible"]
    LeadingNul,
}

#[derive(Params)]
struct BadLabelsParams {
    #[id = "mode"]
    mode: EnumParam<BadLabels>,
}

impl Default for BadLabelsParams {
    fn default() -> Self {
        Self {
            mode: EnumParam::new("Mode", BadLabels::Valid),
        }
    }
}

#[test]
fn typed_enums_need_nonblank_clap_value_text() {
    type P = TestPlugin<BadLabelsParams>;
    let instance = Instance::new::<P>();
    let params = unsafe { inventory(instance.0).unwrap() };
    assert_ne!(params[0].flags & param_flags::STEPPED, 0);
    assert_eq!(params[0].flags & param_flags::ENUM, 0);

    let id = ParamId::new("mode");
    let mut panel = panel(Cell::half(
        0,
        0,
        2,
        Half::Top,
        WidgetKind::Dropdown,
        CellParam::Id(id.clone()),
    ));
    panel.choice_labels.insert(
        id,
        vec![
            bilingual("Valid", "Gültig"),
            bilingual("Blank", "Leer"),
            bilingual("Invisible", "Unsichtbar"),
        ],
    );
    assert!(unsafe { compile::<P>(instance.0, &panel) }.is_err());
}
