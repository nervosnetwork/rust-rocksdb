use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn get_flags_from_detect_platform_script() -> Option<Vec<String>> {
    if !cfg!(target_os = "windows") {
        let mut cmd = Command::new("bash");
        if cfg!(feature = "static") {
            cmd.env("LIB_MODE", "static");
        }
        if cfg!(feature = "portable") {
            cmd.env("PORTABLE", "1");
        }
        if cfg!(feature = "io-uring") {
            cmd.env("ROCKSDB_USE_IO_URING", "1");
        } else {
            cmd.env("ROCKSDB_USE_IO_URING", "0");
        }

        let output = cmd
            .arg("build_detect_platform")
            .output()
            .expect("failed to execute process");
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout);
            if let Ok(ini) = ini::Ini::load_from_str(&raw) {
                if let Some(section) = ini.section(None::<String>) {
                    if let Some(flags_string) = section.get("PLATFORM_CXXFLAGS") {
                        let flags: Vec<String> = flags_string
                            .split(' ')
                            .filter_map(|s| {
                                if !s.is_empty()
                                    && s != "-DZLIB"
                                    && s != "-DBZIP2"
                                    && s != "-DLZ4"
                                    && s != "-DZSTD"
                                    && s != "-DSNAPPY"
                                    && s != "-DROCKSDB_BACKTRACE"
                                {
                                    Some(s.to_owned())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        return Some(flags);
                    }
                }
            }
        }
    }
    None
}

fn link(name: &str, bundled: bool) {
    use std::env::var;
    let target = var("TARGET").unwrap();
    let target: Vec<_> = target.split('-').collect();
    if target.get(2) == Some(&"windows") {
        println!("cargo:rustc-link-lib=dylib={}", name);
        if bundled && target.get(3) == Some(&"gnu") {
            let dir = var("CARGO_MANIFEST_DIR").unwrap();
            println!("cargo:rustc-link-search=native={}/{}", dir, target[0]);
        }
    }
}

fn fail_on_empty_directory(name: &str) {
    if fs::read_dir(name).unwrap().count() == 0 {
        println!(
            "The `{}` directory is empty, did you forget to pull the submodules?",
            name
        );
        println!("Try `git submodule update --init --recursive`");
        panic!();
    }
}

fn bindgen_rocksdb() {
    let bindings = bindgen::Builder::default()
        .header("patches/rocksdb.h")
        .derive_debug(false)
        .blocklist_type("max_align_t") // https://github.com/rust-lang-nursery/rust-bindgen/issues/550
        .ctypes_prefix("libc")
        .size_t_is_usize(true)
        .generate()
        .expect("unable to generate rocksdb bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("unable to write rocksdb bindings");
}

fn build_rocksdb() {
    let target = env::var("TARGET").unwrap();

    let mut config = cc::Build::new();
    config.include("rocksdb/include/");
    config.include("rocksdb/");
    config.include("rocksdb/third-party/gtest-1.8.1/fused-src/");
    config.include("./");

    if cfg!(feature = "snappy") {
        config.define("SNAPPY", Some("1"));
        config.include("snappy/");
    }

    if cfg!(feature = "lz4") {
        config.define("LZ4", Some("1"));
        config.include("lz4/lib/");
    }

    if cfg!(feature = "zstd") {
        config.define("ZSTD", Some("1"));
        config.include("zstd/lib/");
        config.include("zstd/lib/dictBuilder/");
    }

    if cfg!(feature = "zlib") {
        config.define("ZLIB", Some("1"));
        config.include("zlib/");
    }

    if cfg!(feature = "bzip2") {
        config.define("BZIP2", Some("1"));
        config.include("bzip2/");
    }

    config.include(".");
    config.define("NDEBUG", Some("1"));
    // Explicitly disable stats and perf
    config.define("NIOSTATS_CONTEXT", None);
    config.define("NPERF_CONTEXT", None);

    let mut lib_sources = include_str!("rocksdb_lib_sources.txt")
        .trim()
        .split('\n')
        .map(str::trim)
        .collect::<Vec<&'static str>>();

    // We have a pregenerated a version of build_version.cc in the local directory
    lib_sources = lib_sources
        .iter()
        .cloned()
        .filter(|&file| file != "util/build_version.cc")
        .collect::<Vec<&'static str>>();

    if let Some(flags) = get_flags_from_detect_platform_script() {
        println!("PLATFORM_CXXFLAGS: {:?}", flags);
        for flag in flags {
            if flag == "-DROCKSDB_IOURING_PRESENT" {
                // "-luring" not on PLATFORM_CXXFLAGS but on PLATFORM_LDFLAGS
                // so rust binding build.rs should do it by self
                config.flag("-luring");
                println!("cargo:rustc-link-lib=uring");
            }
            config.flag(&flag);
        }
    } else {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        if !cfg!(feature = "portable") {
            if is_x86_feature_detected!("sse4.2") {
                config.flag_if_supported("-msse4.2");
                config.define("HAVE_SSE42", None);
            }
            if is_x86_feature_detected!("avx2") {
                config.flag_if_supported("-mavx2");
                config.define("HAVE_AVX2", None);
            }
            if is_x86_feature_detected!("bmi2") {
                config.define("HAVE_BMI", None);
            }

            if !target.contains("android") {
                if is_x86_feature_detected!("pclmulqdq") {
                    config.define("HAVE_PCLMUL", None);
                    config.flag_if_supported("-mpclmul");
                }
            }
        }
        if target.contains("darwin") {
            config.define("OS_MACOSX", None);
            config.define("ROCKSDB_PLATFORM_POSIX", None);
            config.define("ROCKSDB_LIB_IO_POSIX", None);
        } else if target.contains("android") {
            config.define("OS_ANDROID", None);
            config.define("ROCKSDB_PLATFORM_POSIX", None);
            config.define("ROCKSDB_LIB_IO_POSIX", None);
        } else if target.contains("linux") {
            config.define("OS_LINUX", None);
            config.define("ROCKSDB_PLATFORM_POSIX", None);
            config.define("ROCKSDB_LIB_IO_POSIX", None);
        } else if target.contains("freebsd") {
            config.define("OS_FREEBSD", None);
            config.define("ROCKSDB_PLATFORM_POSIX", None);
            config.define("ROCKSDB_LIB_IO_POSIX", None);
        }
        config.flag(&cxx_standard());
    }

    if target.contains("aarch64") {
        lib_sources.push("util/crc32c_arm64.cc")
    }

    if target.contains("windows") {
        link("rpcrt4", false);
        link("shlwapi", false);
        config.define("DWIN32", None);
        config.define("OS_WIN", None);
        config.define("_MBCS", None);
        config.define("WIN64", None);
        config.define("NOMINMAX", None);
        config.define("ROCKSDB_WINDOWS_UTF8_FILENAMES", None);

        if &target == "x86_64-pc-windows-gnu" {
            // Tell MinGW to create localtime_r wrapper of localtime_s function.
            config.define("_POSIX_C_SOURCE", Some("1"));
            // Tell MinGW to use at least Windows Vista headers instead of the ones of Windows XP.
            // (This is minimum supported version of rocksdb)
            config.define("_WIN32_WINNT", Some("_WIN32_WINNT_VISTA"));
        }

        // Remove POSIX-specific sources
        lib_sources = lib_sources
            .iter()
            .cloned()
            .filter(|file| {
                !matches!(
                    *file,
                    "port/port_posix.cc"
                        | "env/env_posix.cc"
                        | "env/fs_posix.cc"
                        | "env/io_posix.cc"
                )
            })
            .collect::<Vec<&'static str>>();

        // Add Windows-specific sources
        lib_sources.extend([
            "port/win/env_default.cc",
            "port/win/port_win.cc",
            "port/win/xpress_win.cc",
            "port/win/io_win.cc",
            "port/win/win_thread.cc",
            "port/win/env_win.cc",
            "port/win/win_logger.cc",
        ]);

        if cfg!(feature = "jemalloc") {
            lib_sources.push("port/win/win_jemalloc.cc");
        }
    }

    if target.contains("msvc") {
        config.flag("-EHsc");
        config.flag("-std:c++17");
    } else {
        // matches the flags in CMakeLists.txt from rocksdb
        config.flag("-Wsign-compare");
        config.flag("-Wshadow");
        config.flag("-Wno-unused-parameter");
        config.flag("-Wno-unused-variable");
        config.flag("-Woverloaded-virtual");
        config.flag("-Wnon-virtual-dtor");
        config.flag("-Wno-missing-field-initializers");
        config.flag("-Wno-strict-aliasing");
        config.flag("-Wno-invalid-offsetof");

        if cfg!(feature = "jemalloc") {
            config.define("ROCKSDB_JEMALLOC", None);
            config.define("JEMALLOC_NO_DEMANGLE", None);
        }
    }

    for file in lib_sources {
        let file = "rocksdb/".to_string() + file;
        config.file(&file);
    }

    config.file("patches/rocksdb.cc");
    config.file("build_version.cc");

    config.cpp(true);
    config.compile("librocksdb.a");
}

fn build_snappy() {
    let target = env::var("TARGET").unwrap();
    let endianness = env::var("CARGO_CFG_TARGET_ENDIAN").unwrap();
    let mut config = cc::Build::new();

    config.include("snappy/");
    config.include(".");
    config.define("NDEBUG", Some("1"));
    config.extra_warnings(false);

    if target.contains("msvc") {
        config.flag("-EHsc");
    } else {
        // Snappy requires C++11.
        // See: https://github.com/google/snappy/blob/master/CMakeLists.txt#L32-L38
        config.flag("-std=c++11");
    }

    if endianness == "big" {
        config.define("SNAPPY_IS_BIG_ENDIAN", Some("1"));
    }

    config.file("snappy/snappy.cc");
    config.file("snappy/snappy-sinksource.cc");
    config.file("snappy/snappy-c.cc");
    config.cpp(true);
    config.compile("libsnappy.a");
}

fn build_lz4() {
    let mut compiler = cc::Build::new();

    compiler
        .file("lz4/lib/lz4.c")
        .file("lz4/lib/lz4frame.c")
        .file("lz4/lib/lz4hc.c")
        .file("lz4/lib/xxhash.c");

    compiler.opt_level(3);

    let target = env::var("TARGET").unwrap();

    if &target == "i686-pc-windows-gnu" {
        compiler.flag("-fno-tree-vectorize");
    }

    compiler.compile("liblz4.a");
}

fn build_zstd() {
    let mut compiler = cc::Build::new();

    compiler.include("zstd/lib/");
    compiler.include("zstd/lib/common");
    compiler.include("zstd/lib/legacy");

    let globs = &[
        "zstd/lib/common/*.c",
        "zstd/lib/compress/*.c",
        "zstd/lib/decompress/*.c",
        "zstd/lib/dictBuilder/*.c",
        "zstd/lib/legacy/*.c",
    ];

    for pattern in globs {
        for path in glob::glob(pattern).unwrap() {
            let path = path.unwrap();
            compiler.file(path);
        }
    }

    compiler.opt_level(3);
    compiler.extra_warnings(false);

    compiler.define("ZSTD_LIB_DEPRECATED", Some("0"));
    compiler.compile("libzstd.a");
}

fn build_zlib() {
    let mut compiler = cc::Build::new();

    let globs = &["zlib/*.c"];

    for pattern in globs {
        for path in glob::glob(pattern).unwrap() {
            let path = path.unwrap();
            compiler.file(path);
        }
    }

    compiler.flag_if_supported("-Wno-implicit-function-declaration");
    compiler.opt_level(3);
    compiler.extra_warnings(false);
    compiler.compile("libz.a");
}

fn build_bzip2() {
    let mut compiler = cc::Build::new();

    compiler
        .file("bzip2/blocksort.c")
        .file("bzip2/bzlib.c")
        .file("bzip2/compress.c")
        .file("bzip2/crctable.c")
        .file("bzip2/decompress.c")
        .file("bzip2/huffman.c")
        .file("bzip2/randtable.c");

    compiler
        .define("_FILE_OFFSET_BITS", Some("64"))
        .define("BZ_NO_STDIO", None);

    compiler.extra_warnings(false);
    compiler.opt_level(3);
    compiler.extra_warnings(false);
    compiler.compile("libbz2.a");
}

fn try_to_find_and_link_lib(lib_name: &str) -> bool {
    if let Ok(v) = env::var(&format!("{}_COMPILE", lib_name)) {
        if v.to_lowercase() == "true" || v == "1" {
            return false;
        }
    }

    if let Ok(lib_dir) = env::var(&format!("{}_LIB_DIR", lib_name)) {
        println!("cargo:rustc-link-search=native={}", lib_dir);
        let mode = match env::var_os(&format!("{}_STATIC", lib_name)) {
            Some(_) => "static",
            None => "dylib",
        };
        println!("cargo:rustc-link-lib={}={}", mode, lib_name.to_lowercase());
        return true;
    }
    false
}

fn cxx_standard() -> String {
    env::var("ROCKSDB_CXX_STD").map_or("-std=c++17".to_owned(), |cxx_std| {
        if !cxx_std.starts_with("-std=") {
            format!("-std={}", cxx_std)
        } else {
            cxx_std
        }
    })
}

fn main() {
    bindgen_rocksdb();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=rocksdb/");
    println!("cargo:rerun-if-changed=patches/");
    fail_on_empty_directory("rocksdb");
    build_rocksdb();

    if cfg!(feature = "snappy") && !try_to_find_and_link_lib("SNAPPY") {
        println!("cargo:rerun-if-changed=snappy/");
        fail_on_empty_directory("snappy");
        build_snappy();
    }
    if cfg!(feature = "lz4") && !try_to_find_and_link_lib("LZ4") {
        println!("cargo:rerun-if-changed=lz4/");
        fail_on_empty_directory("lz4");
        build_lz4();
    }
    if cfg!(feature = "zstd") && !try_to_find_and_link_lib("ZSTD") {
        println!("cargo:rerun-if-changed=zstd/");
        fail_on_empty_directory("zstd");
        build_zstd();
    }
    if cfg!(feature = "zlib") && !try_to_find_and_link_lib("Z") {
        println!("cargo:rerun-if-changed=zlib/");
        fail_on_empty_directory("zlib");
        build_zlib();
    }
    if cfg!(feature = "bzip2") && !try_to_find_and_link_lib("BZ2") {
        println!("cargo:rerun-if-changed=bzip2/");
        fail_on_empty_directory("bzip2");
        build_bzip2();
    }
}
