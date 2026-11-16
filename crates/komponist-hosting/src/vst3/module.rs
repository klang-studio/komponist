//! Loading a `.vst3` module: the bundle layout, `dlopen`, the module entry and exit functions, the
//! plugin factory and its class list.
//!
//! Host duties (VST3 module spec, spike WP-15 §1.7): call the entry function right after `dlopen`
//! and the exit function right before `dlclose`, and keep the library mapped until every COM
//! object it created is released. The entry and exit functions are per OS:
//!
//! | OS | Binary | Entry | Exit |
//! |---|---|---|---|
//! | Linux | `X.vst3/Contents/<arch>-linux/X.so` | `ModuleEntry(dlopen handle)` | `ModuleExit()` |
//! | macOS | `X.vst3/Contents/MacOS/X` (Mach-O, named by `Info.plist`) | `bundleEntry(CFBundleRef)` | `bundleExit()` |
//! | Windows | `X.vst3/Contents/x86_64-win/X.vst3` (a DLL), or a single-file `X.vst3` DLL | `InitDll()` | `ExitDll()` |
//!
//! On Windows both functions are optional (the module spec says so; most modules export them).
//!
//! On macOS the `CFBundleRef` stays alive until after `bundleExit` (plugins keep it to find their
//! resources). A [`Module`] is shared through an [`Arc`] by everything that holds a COM pointer
//! into the library (the plugin, its processor), and modules are cached per binary, so two
//! instances of one bundle in a process share one entry / exit pair.
//!
//! The library is opened through [`crate::library`], so its code stays mapped after the exit
//! function and `dlclose` (see there why); loading the module again afterwards calls the entry
//! function again.

#[cfg(unix)]
use std::ffi::c_void;
use std::ffi::{CStr, c_char};
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

/// The module entry and exit symbols of this OS (see the module docs).
#[cfg(target_os = "linux")]
const ENTRY_SYMBOL: &CStr = c"ModuleEntry";
#[cfg(target_os = "linux")]
const EXIT_SYMBOL: &CStr = c"ModuleExit";
#[cfg(target_os = "macos")]
const ENTRY_SYMBOL: &CStr = c"bundleEntry";
#[cfg(target_os = "macos")]
const EXIT_SYMBOL: &CStr = c"bundleExit";

#[cfg(windows)]
const ENTRY_SYMBOL: &CStr = c"InitDll";
#[cfg(windows)]
const EXIT_SYMBOL: &CStr = c"ExitDll";

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!(
    "komponist-hosting: no VST3 bundle layout or module entry (vst3/module.rs) for this OS"
);

/// The shared library of a bundle (see the module docs for the layouts). A `.vst3` that is a plain
/// file (the pre-3.6.10 layout) is the library itself.
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
    bundle_binary(bundle)
}

/// macOS: `Contents/MacOS/<CFBundleExecutable>`, else `Contents/MacOS/<bundle name>`.
#[cfg(target_os = "macos")]
fn bundle_binary(bundle: &Path) -> Result<PathBuf, HostingError> {
    crate::bundle::executable(bundle).ok_or_else(|| {
        HostingError::Load(format!(
            "{}: no executable in Contents/MacOS",
            bundle.display()
        ))
    })
}

/// Linux: `Contents/<arch>-linux/X.so`, or any single `.so` in that directory when it is named
/// differently.
#[cfg(target_os = "linux")]
fn bundle_binary(bundle: &Path) -> Result<PathBuf, HostingError> {
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

/// Windows: `Contents/<arch>-win/X.vst3` (`x86_64-win`, `arm64-win` on ARM), or any single
/// `.vst3` file in that directory when it is named differently.
#[cfg(windows)]
fn bundle_binary(bundle: &Path) -> Result<PathBuf, HostingError> {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        arch => arch,
    };
    let dir = bundle.join("Contents").join(format!("{arch}-win"));
    if let Some(name) = bundle.file_name() {
        let preferred = dir.join(name);
        if preferred.is_file() {
            return Ok(preferred);
        }
    }
    let mut libraries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| HostingError::Load(format!("{}: {e}", dir.display())))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("vst3"))
                && path.is_file()
        })
        .collect();
    libraries.sort();
    libraries
        .into_iter()
        .next()
        .ok_or_else(|| HostingError::Load(format!("{}: no module DLL", dir.display())))
}

/// One loaded VST3 library with its entry function done. Dropping it calls the exit function and
/// `dlclose` (which leaves the code mapped, see [`crate::library`]).
#[derive(Debug)]
pub(crate) struct Module {
    binary: PathBuf,
    library: Library,
    entered: bool,
    /// The bundle handed to `bundleEntry`: released after `bundleExit` and `dlclose` (declared
    /// after `library`).
    #[cfg(target_os = "macos")]
    bundle: Option<crate::bundle::CfBundle>,
}

/// `ModuleEntry(void* sharedLibraryHandle)` (Linux) or `bundleEntry(CFBundleRef)` (macOS).
#[cfg(unix)]
type ModuleEntryFn = unsafe extern "C" fn(*mut c_void) -> bool;
/// `InitDll()` (Windows; `PLUGIN_API` is the system calling convention).
#[cfg(windows)]
type ModuleEntryFn = unsafe extern "system" fn() -> bool;
/// `ModuleExit()` (Linux), `bundleExit()` (macOS) or `ExitDll()` (Windows).
type ModuleExitFn = unsafe extern "system" fn() -> bool;
type GetPluginFactoryFn = unsafe extern "system" fn() -> *mut IPluginFactory;

impl Module {
    fn open(bundle: &Path, binary: &Path) -> Result<Module, HostingError> {
        // SAFETY: loading a plugin library runs foreign code; the bridge process exists to contain
        // it.
        let library = unsafe { Library::open(binary) }?;
        #[cfg(not(target_os = "macos"))]
        let _ = bundle;
        let mut module = Module {
            binary: binary.to_path_buf(),
            library,
            entered: false,
            #[cfg(target_os = "macos")]
            bundle: crate::bundle::CfBundle::open(bundle),
        };
        // Older modules do not export it; the spec makes it mandatory only for new ones.
        // SAFETY: the VST3 module spec's signature.
        if let Some(entry) = unsafe { module.library.symbol::<ModuleEntryFn>(ENTRY_SYMBOL) } {
            #[cfg(unix)]
            let argument = module.entry_argument()?;
            // SAFETY: called once after dlopen with the dlopen handle (Linux) or the module's
            // bundle (macOS), or without an argument (Windows), as the VST3 module spec says.
            #[cfg(unix)]
            let entered = unsafe { entry(argument) };
            #[cfg(windows)]
            let entered = unsafe { entry() };
            if !entered {
                return Err(HostingError::Load(format!(
                    "{}: {} failed",
                    binary.display(),
                    ENTRY_SYMBOL.to_string_lossy()
                )));
            }
            module.entered = true;
        }
        Ok(module)
    }

    /// What the entry function gets: the dlopen handle.
    #[cfg(target_os = "linux")]
    fn entry_argument(&self) -> Result<*mut c_void, HostingError> {
        Ok(self.library.handle())
    }

    /// What the entry function gets: the bundle (a single-file module has none).
    #[cfg(target_os = "macos")]
    fn entry_argument(&self) -> Result<*mut c_void, HostingError> {
        self.bundle
            .as_ref()
            .map(crate::bundle::CfBundle::as_ptr)
            .ok_or_else(|| {
                HostingError::Load(format!(
                    "{}: bundleEntry needs a bundle directory",
                    self.binary.display()
                ))
            })
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
            && let Some(exit) = unsafe { self.library.symbol::<ModuleExitFn>(EXIT_SYMBOL) }
        {
            // SAFETY: balanced with the successful entry call; every COM object of this library
            // is released by now (they hold the `Arc<Module>`). `dlclose` follows with `library`,
            // the bundle (macOS) is released after it.
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
    let module = Arc::new(Module::open(bundle, &binary)?);
    modules.push((binary, Arc::downgrade(&module)));
    Ok(module)
}

/// Byte `i` of the canonical class id is byte `CANONICAL[i]` of a `TUID` in memory. On Windows the
/// SDK lays a `TUID` out as a COM `GUID` (`COM_COMPATIBLE`: `Data1` little-endian `u32`, `Data2`
/// and `Data3` little-endian `u16`, then 8 bytes), elsewhere in plain byte order. The permutation is
/// its own inverse.
#[cfg(windows)]
const CANONICAL: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
#[cfg(not(windows))]
const CANONICAL: [usize; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// A class id as komponist's plugin id: 32 upper-case hex digits in the canonical byte order
/// (the order `FUID::toString` prints and Linux and macOS keep in memory), so a project names a
/// VST3 plugin the same way on every OS.
pub(crate) fn cid_hex(cid: &TUID) -> String {
    cid_to_canonical(cid)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// The bytes of a class id in the canonical order (see [`cid_hex`]).
pub(crate) fn cid_to_canonical(cid: &TUID) -> [u8; 16] {
    CANONICAL.map(|i| cid[i] as u8)
}

/// A class id from its bytes in the canonical order.
pub(crate) fn cid_from_canonical(bytes: &[u8; 16]) -> TUID {
    CANONICAL.map(|i| bytes[i] as c_char)
}

/// Parses a plugin id from [`cid_hex`] (case-insensitive).
pub(crate) fn parse_cid(id: &str) -> Option<TUID> {
    if id.len() != 32 || !id.is_ascii() {
        return None;
    }
    let mut cid: TUID = [0; 16];
    for (i, &at) in CANONICAL.iter().enumerate() {
        cid[at] = u8::from_str_radix(&id[2 * i..2 * i + 2], 16).ok()? as c_char;
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
                sidechain: false,
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
        // The id the SDK's `uid` macro makes from "kpTe" "stSy" "nth0" "0001" (a COM GUID on
        // Windows).
        let mut cid: TUID = b"kpTestSynth00001".map(|b| b as c_char);
        if cfg!(windows) {
            cid = CANONICAL.map(|i| cid[i]);
        }
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

    #[cfg(target_os = "linux")]
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

    #[cfg(windows)]
    #[test]
    fn bundle_layout() {
        let dir = std::env::temp_dir().join(format!("kp-vst3-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let arch = dir.join(r"Odd Name.vst3\Contents\x86_64-win");
        std::fs::create_dir_all(&arch).unwrap();
        let bundle = dir.join("Odd Name.vst3");
        assert!(matches!(binary_path(&bundle), Err(HostingError::Load(_))));
        std::fs::write(arch.join("other.VST3"), b"").unwrap();
        assert_eq!(binary_path(&bundle).unwrap(), arch.join("other.VST3"));
        std::fs::write(arch.join("Odd Name.vst3"), b"").unwrap();
        assert_eq!(binary_path(&bundle).unwrap(), arch.join("Odd Name.vst3"));
        // A legacy single-file module, and a path that does not exist.
        let file = dir.join("Single.vst3");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(binary_path(&file).unwrap(), file);
        assert!(matches!(
            binary_path(&dir.join("Missing.vst3")),
            Err(HostingError::Load(_))
        ));
        // Not a DLL: LoadLibraryExW fails cleanly.
        assert!(matches!(
            load_module(&bundle),
            Err(HostingError::Load(m)) if m.contains("Odd Name.vst3")
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundle_layout() {
        let dir = std::env::temp_dir().join(format!("kp-vst3-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = dir.join("Odd Name.vst3");
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        // The Linux layout is no macOS bundle.
        let linux = bundle
            .join("Contents")
            .join(format!("{}-linux", std::env::consts::ARCH));
        std::fs::create_dir_all(&linux).unwrap();
        std::fs::write(linux.join("Odd Name.so"), b"").unwrap();
        assert!(matches!(binary_path(&bundle), Err(HostingError::Load(_))));
        std::fs::write(macos.join("Odd Name"), b"").unwrap();
        assert_eq!(binary_path(&bundle).unwrap(), macos.join("Odd Name"));
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
            Err(HostingError::Load(m)) if m.contains("Odd Name")
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
