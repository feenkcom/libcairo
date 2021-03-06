use crate::pixman_library::PixmanLibrary;
use libfreetype_library::{libfreetype, libpng, libzlib};
use shared_library_builder::{
    Library, LibraryCompilationContext, LibraryDependencies, LibraryLocation, LibraryOptions,
    TarArchive, TarUrlLocation,
};
use serde::{Serialize, Deserialize};

use std::error::Error;
use std::fs::{read_to_string, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use user_error::UserFacingError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CairoLibrary {
    source_location: LibraryLocation,
    release_location: Option<LibraryLocation>,
    dependencies: LibraryDependencies,
    options: LibraryOptions,
}

impl Default for CairoLibrary {
    fn default() -> Self {
        Self::new()
    }
}

impl CairoLibrary {
    pub fn new() -> Self {
        Self {
            source_location: LibraryLocation::Tar(
                TarUrlLocation::new("https://dl.feenk.com/cairo/cairo-1.17.4.tar.xz")
                    .archive(TarArchive::Xz)
                    .sources(Path::new("cairo-1.17.4")),
            ),
            release_location: None,
            dependencies: LibraryDependencies::new()
                .push(PixmanLibrary::new().into())
                .push(libfreetype(None as Option<String>).into()),
            options: LibraryOptions::default(),
        }
    }

    pub fn with_release_location(mut self, release_location: Option<LibraryLocation>) -> Self {
        self.release_location = release_location;
        self
    }

    fn compile_unix(&self, context: &LibraryCompilationContext) -> Result<(), Box<dyn Error>> {
        self.patch_unix_makefile(context)?;

        let freetype = libfreetype(None as Option<String>);

        let out_dir = self.native_library_prefix(context);
        if !out_dir.exists() {
            std::fs::create_dir_all(&out_dir)
                .unwrap_or_else(|_| panic!("Could not create {:?}", &out_dir));
        }
        let makefile_dir = out_dir.clone();

        let mut pkg_config_paths = self.all_pkg_config_directories(context);
        pkg_config_paths.push(PathBuf::from("../pixman"));
        if let Ok(ref path) = std::env::var("PKG_CONFIG_PATH") {
            std::env::split_paths(path).for_each(|path| pkg_config_paths.push(path));
        }

        let mut cpp_flags = std::env::var("CPPFLAGS").unwrap_or_else(|_| "".to_owned());
        cpp_flags = format!(
            "{} {}",
            cpp_flags,
            self.dependencies.include_headers_flags(context),
        );

        let mut linker_flags = std::env::var("LDFLAGS").unwrap_or_else(|_| "".to_owned());
        linker_flags = format!("{} {} -lbz2_static", linker_flags, self.dependencies.linker_libraries_flags(context));

        println!("cpp_flags = {}", &cpp_flags);
        println!("linker_flags = {}", &linker_flags);

        let mut command = Command::new(self.source_directory(context).join("configure"));
        command
            .current_dir(&out_dir)
            .env(
                "PKG_CONFIG_PATH",
                std::env::join_paths(&pkg_config_paths).unwrap(),
            )
            .env(
                "FREETYPE_CONFIG",
                freetype
                    .pkg_config_directory(context)
                    .expect("Could not find freetype's pkgconfig"),
            )
            .env("CPPFLAGS", &cpp_flags)
            .env("LDFLAGS", &linker_flags)
            .arg("--enable-ft=yes")
            .arg(format!(
                "--prefix={}",
                self.native_library_prefix(context).display()
            ))
            .arg(format!(
                "--exec-prefix={}",
                self.native_library_prefix(context).display()
            ))
            .arg(format!(
                "--libdir={}",
                self.native_library_prefix(context).join("lib").display()
            ));

        println!("{:?}", &command);

        let configure = command.status().unwrap();

        if !configure.success() {
            panic!("Could not configure {}", self.name());
        }

        let mut command = Command::new("make");
        command
            .current_dir(&makefile_dir)
            .arg("install")
            .env(
                "PKG_CONFIG_PATH",
                std::env::join_paths(&pkg_config_paths).unwrap(),
            )
            .env(
                "FREETYPE_CONFIG",
                freetype
                    .pkg_config_directory(context)
                    .expect("Could not find freetype's pkgconfig"),
            )
            .env("CPPFLAGS", &cpp_flags)
            .env("LDFLAGS", &linker_flags);

        println!("{:?}", &command);

        let make = command.status().unwrap();

        if !make.success() {
            panic!("Could not compile {}", self.name());
        }

        Ok(())
    }

    fn compile_windows(&self, options: &LibraryCompilationContext) -> Result<(), Box<dyn Error>> {
        self.patch_windows_common_makefile(options)?;
        self.patch_windows_features_makefile(options)?;
        self.patch_windows_makefile(options)?;

        let makefile = self.source_directory(options).join("Makefile.win32");

        let mut command = Command::new("make");
        command
            .current_dir(self.source_directory(options))
            .arg("cairo")
            .arg("-f")
            .arg(&makefile)
            .arg("CFG=release")
            .arg(format!(
                "PIXMAN_PATH={}",
                PixmanLibrary::new()
                    .native_library_prefix(options)
                    .display()
            ))
            .arg(format!(
                "ZLIB_PATH={}",
                libzlib().native_library_prefix(options).display()
            ))
            .arg(format!(
                "LIBPNG_PATH={}",
                libpng().native_library_prefix(options).display()
            ));

        println!("{:?}", &command);

        let configure = command.status().unwrap();

        if !configure.success() {
            panic!("Could not configure {}", self.name());
        }
        Ok(())
    }

    fn patch_file_with(
        &self,
        path: impl AsRef<Path>,
        patcher: impl FnOnce(String) -> String,
    ) -> Result<(), Box<dyn Error>> {
        let path = path.as_ref().to_path_buf();
        let file_name = path
            .file_name()
            .ok_or_else(|| UserFacingError::new("Could not get file name"))?
            .to_os_string();

        let mut fixed_file_name = file_name.clone();
        fixed_file_name.push(".fixed");
        let mut backup_file_name = file_name;
        backup_file_name.push(".bak");

        let parent_directory = path
            .parent()
            .ok_or_else(|| UserFacingError::new("Could not get parent folder"))?;

        let actual_file = path.clone();
        let fixed_file = parent_directory.join(&fixed_file_name);
        let backup_file = parent_directory.join(&backup_file_name);

        if fixed_file.exists() {
            std::fs::remove_file(&fixed_file)?;
            std::fs::copy(&backup_file, &actual_file)?;
        } else {
            std::fs::copy(&actual_file, &backup_file)?;
        }

        let mut contents = read_to_string(&actual_file)?;
        contents = patcher(contents);

        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&actual_file)?;
        file.write_all(contents.as_bytes())?;

        std::fs::copy(&actual_file, &fixed_file)?;

        Ok(())
    }

    fn patch_unix_makefile(
        &self,
        options: &LibraryCompilationContext,
    ) -> Result<(), Box<dyn Error>> {
        self.patch_file_with(
            self.source_directory(options).join("Makefile.in"),
            |contents| {
                contents.replace(
                    "DIST_SUBDIRS = src doc util boilerplate test perf",
                    "DIST_SUBDIRS = src boilerplate",
                )
            },
        )?;
        Ok(())
    }

    fn patch_windows_common_makefile(
        &self,
        options: &LibraryCompilationContext,
    ) -> Result<(), Box<dyn Error>> {
        let freetype = libfreetype(None as Option<String>);

        self.patch_file_with(
            self.source_directory(options)
                .join("build")
                .join("Makefile.win32.common"),
            |contents| {
                let mut contents = contents.replace("-MD", "-MT");
                contents = contents.replace(
                    "CAIRO_LIBS += $(ZLIB_PATH)/zdll.lib",
                    "CAIRO_LIBS += $(ZLIB_PATH)/lib/zlibstatic.lib",
                );

                contents = contents.replace(
                    "ZLIB_CFLAGS += -I$(ZLIB_PATH)",
                    "ZLIB_CFLAGS += -I$(ZLIB_PATH)/include",
                );
                contents = contents.replace(
                    "CAIRO_LIBS +=  $(LIBPNG_PATH)/libpng.lib",
                    "CAIRO_LIBS +=  $(LIBPNG_PATH)/lib/libpng16_static.lib",
                );
                contents = contents.replace(
                    "LIBPNG_CFLAGS += -I$(LIBPNG_PATH)/",
                    "LIBPNG_CFLAGS += -I$(LIBPNG_PATH)/include",
                );

                contents = contents.replace("@mkdir", "@coreutils mkdir");
                contents = contents.replace("`dirname $<`", "\"$(shell coreutils dirname $<)\"");

                let include_flags_to_replace =
                    "DEFAULT_CFLAGS += -I. -I$(top_srcdir) -I$(top_srcdir)/src";

                let mut paths_to_include = self.msvc_include_directories();
                paths_to_include.extend(freetype.native_library_include_headers(options));

                let new_include_flags = paths_to_include
                    .into_iter()
                    .map(|path| format!("DEFAULT_CFLAGS += -I\"{}\"", path.display()))
                    .collect::<Vec<String>>()
                    .join("\n");

                contents = contents.replace(
                    include_flags_to_replace,
                    &format!("{}\n{}", include_flags_to_replace, new_include_flags),
                );

                let ld_flags_to_replace = "DEFAULT_LDFLAGS = -nologo $(CFG_LDFLAGS)";

                let mut paths_to_link = self.msvc_lib_directories();

                paths_to_link.extend(freetype.native_library_linker_libraries(options));

                let new_ld_flags = paths_to_link
                    .into_iter()
                    .map(|path| format!("DEFAULT_LDFLAGS += -LIBPATH:\"{}\"", path.display()))
                    .collect::<Vec<String>>()
                    .join("\n");

                contents = contents.replace(
                    ld_flags_to_replace,
                    &format!("{}\n{}", ld_flags_to_replace, new_ld_flags),
                );

                contents = contents.replace(
                    "CAIRO_LIBS =  gdi32.lib msimg32.lib user32.lib",
                    "CAIRO_LIBS =  gdi32.lib msimg32.lib user32.lib freetype.lib",
                );

                contents
            },
        )?;

        Ok(())
    }

    fn patch_windows_features_makefile(
        &self,
        options: &LibraryCompilationContext,
    ) -> Result<(), Box<dyn Error>> {
        self.patch_file_with(
            self.source_directory(options)
                .join("build")
                .join("Makefile.win32.features-h"),
            |contents| contents.replace("@echo", "@coreutils echo"),
        )?;
        self.patch_file_with(
            self.source_directory(options)
                .join("build")
                .join("Makefile.win32.features"),
            |contents| contents.replace("CAIRO_HAS_FT_FONT=0", "CAIRO_HAS_FT_FONT=1"),
        )?;
        Ok(())
    }

    fn patch_windows_makefile(
        &self,
        options: &LibraryCompilationContext,
    ) -> Result<(), Box<dyn Error>> {
        self.patch_file_with(
            self.source_directory(options)
                .join("src")
                .join("Makefile.win32"),
            |contents| {
                contents.replace(
                    "@for x in $(enabled_cairo_headers); do echo \"	src/$$x\"; done",
                    "",
                )
            },
        )?;

        Ok(())
    }
}

#[typetag::serde]
impl Library for CairoLibrary {
    fn location(&self) -> &LibraryLocation {
        &self.source_location
    }

    fn release_location(&self) -> &LibraryLocation {
        self.release_location
            .as_ref()
            .unwrap_or_else(|| &self.source_location)
    }

    fn name(&self) -> &str {
        "cairo"
    }

    fn ensure_sources(&self, options: &LibraryCompilationContext) -> Result<(), Box<dyn Error>> {
        self.location()
            .ensure_sources(&self.source_directory(options), options)?;
        Ok(())
    }

    fn dependencies(&self) -> Option<&LibraryDependencies> {
        Some(&self.dependencies)
    }

    fn options(&self) -> &LibraryOptions {
        &self.options
    }

    fn options_mut(&mut self) -> &mut LibraryOptions {
        &mut self.options
    }

    fn force_compile(&self, options: &LibraryCompilationContext) -> Result<(), Box<dyn Error>> {
        if options.is_unix() {
            self.compile_unix(options).expect("Failed to compile cairo")
        }
        if options.is_windows() {
            self.compile_windows(options)
                .expect("Failed to compile cairo")
        }
        Ok(())
    }

    fn compiled_library_directories(&self, options: &LibraryCompilationContext) -> Vec<PathBuf> {
        if options.is_unix() {
            let lib = self.native_library_prefix(options).join("lib");
            return vec![lib];
        }
        if options.is_windows() {
            let lib = self
                .native_library_prefix(options)
                .join("src")
                .join(options.profile());
            return vec![lib];
        }
        vec![]
    }

    fn ensure_requirements(&self, options: &LibraryCompilationContext) {
        which::which("make").expect("Could not find `make`");

        if options.is_unix() {
            which::which("autoreconf").expect("Could not find `autoreconf`");
            which::which("aclocal").expect("Could not find `aclocal`");
        }

        if options.is_windows() {
            which::which("coreutils").expect("Could not find `coreutils`");

            for path in self.msvc_lib_directories() {
                if !path.exists() {
                    panic!("Lib folder does not exist: {}", &path.display())
                }
            }
            for path in self.msvc_include_directories() {
                if !path.exists() {
                    panic!("Include folder does not exist: {}", &path.display())
                }
            }
        }
    }

    fn native_library_prefix(&self, options: &LibraryCompilationContext) -> PathBuf {
        if options.is_windows() {
            return self.source_directory(options);
        }

        options.build_root().join(self.name())
    }

    fn native_library_include_headers(&self, context: &LibraryCompilationContext) -> Vec<PathBuf> {
        let mut dirs = vec![];

        let directory = self.native_library_prefix(context).join("include");

        if directory.exists() {
            dirs.push(directory);
        }

        dirs
    }

    fn native_library_linker_libraries(&self, context: &LibraryCompilationContext) -> Vec<PathBuf> {
        let mut dirs = vec![];

        let directory = self.native_library_prefix(context).join("lib");

        if directory.exists() {
            dirs.push(directory);
        }

        dirs
    }

    fn pkg_config_directory(&self, context: &LibraryCompilationContext) -> Option<PathBuf> {
        let directory = self
            .native_library_prefix(context)
            .join("lib")
            .join("pkgconfig");

        if directory.exists() {
            return Some(directory);
        }

        None
    }

    fn clone_library(&self) -> Box<dyn Library> {
        Box::new(Clone::clone(self))
    }
}

impl From<CairoLibrary> for Box<dyn Library> {
    fn from(library: CairoLibrary) -> Self {
        Box::new(library)
    }
}
