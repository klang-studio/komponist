//! Loading a `.vst3` module: the Linux bundle layout, `dlopen`, `ModuleEntry` / `ModuleExit`, the
//! plugin factory and its class list.
//!
//! Host duties (VST3 module spec, spike WP-15 §1.7): call `ModuleEntry(handle)` right after
//! `dlopen` and `ModuleExit()` right before `dlclose`, and keep the library mapped until every COM
//! object it created is released. A [`Module`] is shared through an [`Arc`] by everything that holds
//! a COM pointer into the library (the plugin, its processor), and modules are cached per binary, so
//! two instances of one bundle in a process share one `ModuleEntry` / `ModuleExit` pair.
//!
//! The library is opened through [`crate::library`], so its code stays mapped after `ModuleExit`
//! and `dlclose` (see there why); loading the module again after `ModuleExit` calls `ModuleEntry`
//! again.

use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use komponist_ipc::{PluginDescriptor, PluginFormat};
use vst3::ComPtr;
use vst3::Steinberg::kResultOk;
use vst3::Steinberg::{
    IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactory3, IPluginFactory3Trait,
    IPluginFactoryTrait, PClassInfo, PClassInfo2, PClassInfoW, PFactoryInfo, TUID,
};

use super::strings::{c_chars, utf16};
use crate::library::Library;
use crate::plugin::HostingError;

/// `PClassInfo::category` of the classes komponist hosts (`kVstAudioEffectClass`).
const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// The shared library of a bundle: `X.vst3/Contents/<arch>-linux/X.so`, or any single `.so` in that
/// directory when it is named differently. A `.vst3` that is a plain file (the pre-3.6.10 layout) is
/// the library itself.
pub(crate) fn binary_path(bundle: &Path) -> Result<PathBuf, HostingError> {
    if bundle.is_file() {
        return Ok(bundle.to_path_buf());
    }
    if !bundle.is_dir() {
        return Err(HostingError::Load(format!(
            "{}: no such bundle",
            bundle.display()
        )));
    }
    let dir = bundle
        .join("Contents")
        .join(format!("{}-linux", std::env::consts::ARCH));
    if let Some(stem) = bundle.file_stem() {
        let mut name = stem.to_os_string();
        name.push(".so");
        let preferred = dir.join(name);
        if preferred.is_file() {
            return Ok(preferred);
        }
    }
    let mut libraries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| HostingError::Load(format!("{}: {e}", dir.display())))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "so") && path.is_file())
        .collect();
    libraries.sort();
    libraries
        .into_iter()
        .next()
        .ok_or_else(|| HostingError::Load(format!("{}: no shared library", dir.display())))
}

/// One loaded VST3 library with `ModuleEntry` done. Dropping it calls `ModuleExit` and `dlclose`
/// (which leaves the code mapped, see [`crate::library`]).
#[derive(Debug)]
pub(crate) struct Module {
    binary: PathBuf,
    library: Library,
    entered: bool,
}

type ModuleEntryFn = unsafe extern "C" fn(*mut c_void) -> bool;
type ModuleExitFn = unsafe extern "C" fn() -> bool;
type GetPluginFactoryFn = unsafe extern "C" fn() -> *mut IPluginFactory;

impl Module {
    fn open(binary: &Path) -> Result<Module, HostingError> {
        // SAFETY: loading a plugin library runs foreign code; the bridge process exists to contain
        // it.
        let library = unsafe { Library::open(binary) }?;
        let mut module = Module {
            binary: binary.to_path_buf(),
            library,
            entered: false,
        };
        // Older modules do not export it; the spec makes it mandatory only for new ones.
        // SAFETY: the VST3 module spec's signature.
        if let Some(entry) = unsafe { module.library.symbol::<ModuleEntryFn>(c"ModuleEntry") } {
            // SAFETY: called once after dlopen with the dlopen handle (VST3 module spec).
            if !unsafe { entry(module.library.handle()) } {
                return Err(HostingError::Load(format!(
                    "{}: ModuleEntry failed",
                    binary.display()
                )));
            }
            module.entered = true;
        }
        Ok(module)
    }

    /// The plugin factory (`GetPluginFactory`); each call holds its own reference.
    pub(crate) fn factory(&self) -> Result<ComPtr<IPluginFactory>, HostingError> {
        // SAFETY: the VST3 module spec's signature.
        let get = unsafe {
            self.library
                .symbol::<GetPluginFactoryFn>(c"GetPluginFactory")
        }
        .ok_or_else(|| {
            HostingError::Load(format!("{}: no GetPluginFactory", self.binary.display()))
        })?;
        // SAFETY: the exported factory function; it returns an owned (add-ref'd) pointer or NULL.
        unsafe { ComPtr::from_raw(get()) }.ok_or_else(|| {
            HostingError::Load(format!(
                "{}: GetPluginFactory failed",
                self.binary.display()
            ))
        })
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: the VST3 module spec's signature.
        if self.entered
            && let Some(exit) = unsafe { self.library.symbol::<ModuleExitFn>(c"ModuleExit") }
        {
            // SAFETY: balanced with the successful ModuleEntry; every COM object of this library
            // is released by now (they hold the `Arc<Module>`). `dlclose` follows with `library`.
            unsafe { exit() };
        }
    }
}

/// Live modules by binary path.
static MODULES: Mutex<Vec<(PathBuf, Weak<Module>)>> = Mutex::new(Vec::new());

/// The module of `bundle`, loaded (and entered) if no instance in this process holds it yet.
pub(crate) fn load_module(bundle: &Path) -> Result<Arc<Module>, HostingError> {
    let binary = binary_path(bundle)?;
    let binary = binary.canonicalize().unwrap_or(binary);
    let mut modules = MODULES.lock().unwrap_or_else(PoisonError::into_inner);
    modules.retain(|(_, module)| module.strong_count() > 0);
    if let Some(module) = modules
        .iter()
        .find(|(path, _)| *path == binary)
        .and_then(|(_, module)| module.upgrade())
    {
        return Ok(module);
    }
    let module = Arc::new(Module::open(&binary)?);
    modules.push((binary, Arc::downgrade(&module)));
    Ok(module)
}

/// A class id as komponist's plugin id: 32 upper-case hex digits in byte order.
pub(crate) fn cid_hex(cid: &TUID) -> String {
    cid.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

/// Parses a plugin id from [`cid_hex`] (case-insensitive).
pub(crate) fn parse_cid(id: &str) -> Option<TUID> {
    if id.len() != 32 || !id.is_ascii() {
        return None;
    }
    let mut cid: TUID = [0; 16];
    for (i, byte) in cid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&id[2 * i..2 * i + 2], 16).ok()? as c_char;
    }
    Some(cid)
}

/// The audio module classes of a factory as descriptors, with their class ids.
pub(crate) fn audio_classes(
    factory: &ComPtr<IPluginFactory>,
    bundle: &Path,
) -> Vec<(TUID, PluginDescriptor)> {
    let factory2 = factory.cast::<IPluginFactory2>();
    let factory3 = factory.cast::<IPluginFactory3>();
    // SAFETY: plain out-structs, zeroed like the SDK does.
    let mut info: PFactoryInfo = unsafe { std::mem::zeroed() };
    let factory_vendor = if unsafe { factory.getFactoryInfo(&mut info) } == kResultOk {
        c_chars(&info.vendor)
    } else {
        String::new()
    };
    let bundle = std::path::absolute(bundle).unwrap_or_else(|_| bundle.to_path_buf());
    let count = unsafe { factory.countClasses() };
    let mut classes = Vec::new();
    for index in 0..count.max(0) {
        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        if unsafe { factory.getClassInfo(index, &mut info) } != kResultOk
            || c_chars(&info.category) != AUDIO_MODULE_CLASS
        {
            continue;
        }
        let mut name = c_chars(&info.name);
        let mut vendor = String::new();
        let mut version = String::new();
        let mut subcategories = String::new();
        if let Some(f2) = &factory2 {
            let mut info2: PClassInfo2 = unsafe { std::mem::zeroed() };
            if unsafe { f2.getClassInfo2(index, &mut info2) } == kResultOk {
                vendor = c_chars(&info2.vendor);
                version = c_chars(&info2.version);
                subcategories = c_chars(&info2.subCategories);
            }
        }
        if let Some(f3) = &factory3 {
            let mut info_w: PClassInfoW = unsafe { std::mem::zeroed() };
            if unsafe { f3.getClassInfoUnicode(index, &mut info_w) } == kResultOk {
                let wide = utf16(&info_w.name);
                if !wide.is_empty() {
                    name = wide;
                }
            }
        }
        if vendor.is_empty() {
            vendor = factory_vendor.clone();
        }
        let features: Vec<String> = subcategories
            .split('|')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        classes.push((
            info.cid,
            PluginDescriptor {
                format: PluginFormat::Vst3,
                id: cid_hex(&info.cid),
                name,
                vendor,
                version,
                kind: crate::scan::kind_from_features(PluginFormat::Vst3, &features),
                features,
                bundle: bundle.clone(),
            },
        ));
    }
    classes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_round_trip_as_hex() {
        let cid: TUID = b"kpTestSynth00001".map(|b| b as c_char);
        let hex = cid_hex(&cid);
        assert_eq!(hex, "6B705465737453796E74683030303031");
        assert_eq!(parse_cid(&hex), Some(cid));
        assert_eq!(parse_cid(&hex.to_lowercase()), Some(cid));
        let high: TUID = [-1; 16];
        assert_eq!(cid_hex(&high), "F".repeat(32));
        assert_eq!(parse_cid(&"F".repeat(32)), Some(high));
        for bad in [
            "",
            "6B70",
            &"G".repeat(32),
            &"0".repeat(33),
            "ü".repeat(16).as_str(),
        ] {
            assert_eq!(parse_cid(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn bundle_layout() {
        let dir = std::env::temp_dir().join(format!("kp-vst3-layout-{}", std::process::id()));
        let arch = dir
            .join("Odd Name.vst3/Contents")
            .join(format!("{}-linux", std::env::consts::ARCH));
        std::fs::create_dir_all(&arch).unwrap();
        let bundle = dir.join("Odd Name.vst3");
        assert!(matches!(binary_path(&bundle), Err(HostingError::Load(_))));
        std::fs::write(arch.join("other.so"), b"").unwrap();
        assert_eq!(binary_path(&bundle).unwrap(), arch.join("other.so"));
        std::fs::write(arch.join("Odd Name.so"), b"").unwrap();
        assert_eq!(binary_path(&bundle).unwrap(), arch.join("Odd Name.so"));
        // A single-file module, and a path that does not exist.
        let file = dir.join("Single.vst3");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(binary_path(&file).unwrap(), file);
        assert!(matches!(
            binary_path(&dir.join("Missing.vst3")),
            Err(HostingError::Load(_))
        ));
        // Not a library: dlopen fails cleanly.
        assert!(matches!(
            load_module(&bundle),
            Err(HostingError::Load(m)) if m.contains("Odd Name.so")
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
