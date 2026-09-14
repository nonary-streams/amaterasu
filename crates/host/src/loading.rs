//! Library ownership stays here; policy and feature behavior stay in managed libraries.
use amaterasu_abi::{
    AbiHeader, BorrowedBytes, HostApi, ModuleApi, ModuleEntry, RuntimeApi, RuntimeEntry, STATUS_OK,
    Status,
};
use amaterasu_package::{PackageResult, VerifiedPackage, ordered_components};
use libloading::Library;
use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::path::Path;

pub struct LoadedComponents {
    // Fields drop in declaration order: modules must stop before their runtime disappears.
    modules: Vec<FeatureLibrary>,
    runtime: RuntimeLibrary,
}

impl Drop for LoadedComponents {
    fn drop(&mut self) {
        while let Some(module) = self.modules.pop() {
            drop(module);
        }
    }
}

impl LoadedComponents {
    pub fn load(
        directory: &Path,
        package: &VerifiedPackage,
        progress: &mut dyn FnMut(usize) -> PackageResult<()>,
    ) -> PackageResult<Self> {
        let runtime_component = package
            .manifest
            .components
            .iter()
            .find(|component| component.runtime)
            .ok_or("package has no runtime")?;
        let configuration =
            serde_json::to_vec(&package.runtime_config).map_err(|error| error.to_string())?;
        let mut runtime =
            RuntimeLibrary::load(&directory.join(&runtime_component.file), &configuration)?;
        progress(1)?;
        let mut modules = Vec::new();

        for (index, component) in ordered_components(&package.manifest.components)?
            .into_iter()
            .filter(|component| !component.runtime)
            .enumerate()
        {
            let endpoint = runtime.endpoint(&component.id)?;
            let module =
                FeatureLibrary::load(&directory.join(&component.file), &component.id, &endpoint)?;
            modules.push(module);
            progress(index + 2)?;
        }
        Ok(Self { modules, runtime })
    }

    pub fn tick(&mut self) -> PackageResult<()> {
        for module in &mut self.modules {
            module.tick()?;
        }
        Ok(())
    }

    pub fn health(&self) -> PackageResult<()> {
        for module in &self.modules {
            module.health()?;
        }
        Ok(())
    }

    pub fn console_endpoint(&mut self) -> PackageResult<HostApi> {
        self.runtime.endpoint("amaterasu.console")
    }
}

struct RuntimeLibrary {
    api: RuntimeApi,
    instance: *mut c_void,
    _library: Library,
}

impl RuntimeLibrary {
    fn load(path: &Path, configuration: &[u8]) -> PackageResult<Self> {
        let library = open_verified_library(path)?;

        // SAFETY: the package was verified before executing any library code. The
        // named entry point and table layout are the trusted component's ABI contract.
        let api = unsafe {
            let entry = library
                .get::<RuntimeEntry>(b"amaterasu_runtime_v1\0")
                .map_err(|error| error.to_string())?;
            read_api_table(entry())?
        };
        let mut instance = std::ptr::null_mut();
        // SAFETY: configuration lives throughout create; output points to writable storage.
        let status = unsafe { (api.create)(BorrowedBytes::new(configuration), &mut instance) };
        require_success(status, "create runtime")?;
        if instance.is_null() {
            return Err("runtime returned a null instance".into());
        }
        Ok(Self {
            api,
            instance,
            _library: library,
        })
    }

    fn endpoint(&mut self, principal: &str) -> PackageResult<HostApi> {
        let mut endpoint = MaybeUninit::uninit();
        // SAFETY: instance belongs to this live library; endpoint storage is correctly sized.
        let status = unsafe {
            (self.api.endpoint)(
                self.instance,
                BorrowedBytes::new(principal.as_bytes()),
                endpoint.as_mut_ptr(),
            )
        };
        require_success(status, "create endpoint")?;
        // SAFETY: a successful runtime call initializes the complete endpoint table.
        let endpoint = unsafe { endpoint.assume_init() };
        if !endpoint.header.supports::<HostApi>() {
            return Err("runtime returned an incompatible endpoint".into());
        }
        Ok(endpoint)
    }
}

impl Drop for RuntimeLibrary {
    fn drop(&mut self) {
        // SAFETY: every feature module has already been destroyed by LoadedComponents.
        unsafe { (self.api.destroy)(self.instance) };
    }
}

struct FeatureLibrary {
    id: String,
    api: ModuleApi,
    instance: *mut c_void,
    _library: Library,
}

impl FeatureLibrary {
    fn load(path: &Path, id: &str, endpoint: &HostApi) -> PackageResult<Self> {
        let library = open_verified_library(path)?;
        // SAFETY: the verified native component promises this symbol and C table layout.
        let api = unsafe {
            let entry = library
                .get::<ModuleEntry>(b"amaterasu_module_v1\0")
                .map_err(|error| error.to_string())?;
            read_api_table(entry())?
        };
        let mut instance = std::ptr::null_mut();
        // SAFETY: the module borrows the table during creation and copies it if needed later.
        let status = unsafe { (api.create)(endpoint, &mut instance) };
        require_success(status, &format!("create {id}"))?;
        if instance.is_null() {
            return Err(format!("module {id} returned a null instance"));
        }
        Ok(Self {
            id: id.to_owned(),
            api,
            instance,
            _library: library,
        })
    }

    fn tick(&mut self) -> PackageResult<()> {
        // SAFETY: this instance is exclusively borrowed, live, and owned by this library.
        let status = unsafe { (self.api.tick)(self.instance) };
        require_success(status, &format!("tick {}", self.id))
    }

    fn health(&self) -> PackageResult<()> {
        // SAFETY: no tick runs concurrently and the instance remains alive.
        let status = unsafe { (self.api.health)(self.instance) };
        require_success(status, &format!("health {}", self.id))
    }
}

impl Drop for FeatureLibrary {
    fn drop(&mut self) {
        // SAFETY: the host relinquishes this instance exactly once before unloading its library.
        unsafe { (self.api.destroy)(self.instance) };
    }
}

fn open_verified_library(path: &Path) -> PackageResult<Library> {
    let absolute_path = path.canonicalize().map_err(|error| error.to_string())?;

    #[cfg(windows)]
    {
        const SEARCH_DLL_DIRECTORY: u32 = 0x100;
        const SEARCH_DEFAULT_DIRECTORIES: u32 = 0x1000;
        // SAFETY: only a verified package artifact is loaded; restrict dependency search.
        unsafe {
            libloading::os::windows::Library::load_with_flags(
                absolute_path,
                SEARCH_DLL_DIRECTORY | SEARCH_DEFAULT_DIRECTORIES,
            )
        }
        .map(Library::from)
        .map_err(|error| error.to_string())
    }

    #[cfg(not(windows))]
    {
        // SAFETY: the caller verified the complete package before this constructor can run.
        unsafe { Library::new(absolute_path) }.map_err(|error| error.to_string())
    }
}

unsafe fn read_api_table<T: Copy>(table: *const T) -> PackageResult<T> {
    if table.is_null() {
        return Err("library returned a null API table".into());
    }
    // SAFETY: every API table begins with an AbiHeader; check it before reading the tail.
    let header = unsafe { table.cast::<AbiHeader>().read() };
    if !header.supports::<T>() {
        return Err("incompatible library ABI version or table size".into());
    }
    // SAFETY: the trusted library advertises a complete compatible table.
    Ok(unsafe { table.read() })
}

fn require_success(status: Status, operation: &str) -> PackageResult<()> {
    if status != STATUS_OK {
        return Err(format!("{operation} failed with ABI status {status}"));
    }
    Ok(())
}
