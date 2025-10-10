#![deny(warnings)]

use {
    anyhow::{anyhow, bail, Context, Result},
    std::{
        env,
        fmt::Write as _,
        fs::{self, File},
        io::{self, Write},
        iter,
        path::{Path, PathBuf},
        process::Command,
    },
    tar::Builder,
    zstd::Encoder,
};

const ZSTD_COMPRESSION_LEVEL: i32 = 19;

#[cfg(any(target_os = "macos", target_os = "windows"))]
const PYTHON_EXECUTABLE: &str = "python.exe";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const PYTHON_EXECUTABLE: &str = "python";

#[cfg(target_os = "windows")]
const CLANG_EXECUTABLE: &str = "clang.exe";
#[cfg(not(target_os = "windows"))]
const CLANG_EXECUTABLE: &str = "clang";

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: OUT_DIR = {}",
        out_dir.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Current working directory = {}",
        env::current_dir().unwrap().display()
    );

    if matches!(env::var("CARGO_CFG_FEATURE").as_deref(), Ok("cargo-clippy"))
        || env::var("CLIPPY_ARGS").is_ok()
        || env::var("CARGO_EXPAND_NO_RUN_NIGHTLY").is_ok()
    {
        println!("cargo:warning=BUILD DIAGNOSTIC: Using stubs for clippy");
        stubs_for_clippy(&out_dir)
    } else {
        println!("cargo:warning=BUILD DIAGNOSTIC: Running full build (package_all_the_things)");
        package_all_the_things(&out_dir)
    }?;

    // TODO: how can we detect `cargo test` and only run this in that case (or more specifically, run it so it
    // generates an empty file)?
    test_generator::generate()
}

fn stubs_for_clippy(out_dir: &Path) -> Result<()> {
    println!(
        "cargo:warning=using stubbed runtime, core library, and adapter for static analysis purposes..."
    );

    let files = [
        "libcomponentize_py_runtime.so.zst",
        "libpython3.14.so.zst",
        "libc.so.zst",
        "libwasi-emulated-mman.so.zst",
        "libwasi-emulated-process-clocks.so.zst",
        "libwasi-emulated-getpid.so.zst",
        "libwasi-emulated-signal.so.zst",
        "libc++.so.zst",
        "libc++abi.so.zst",
    ];

    for file in files {
        let path = out_dir.join(file);

        if !path.exists() {
            Encoder::new(File::create(path)?, ZSTD_COMPRESSION_LEVEL)?.do_finish()?;
        }
    }

    let path = out_dir.join("python-lib.tar.zst");

    if !path.exists() {
        Builder::new(Encoder::new(File::create(path)?, ZSTD_COMPRESSION_LEVEL)?)
            .into_inner()?
            .do_finish()?;
    }

    let path = out_dir.join("bundled.tar.zst");

    if !path.exists() {
        Builder::new(Encoder::new(File::create(path)?, ZSTD_COMPRESSION_LEVEL)?)
            .into_inner()?
            .do_finish()?;
    }

    Ok(())
}

fn package_all_the_things(out_dir: &Path) -> Result<()> {
    let repo_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: CARGO_MANIFEST_DIR = {}",
        repo_dir.display()
    );

    let wasi_sdk =
        PathBuf::from(env::var_os("WASI_SDK_PATH").unwrap_or_else(|| "/opt/wasi-sdk".into()));
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: WASI_SDK_PATH = {}",
        wasi_sdk.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: WASI SDK exists: {}",
        wasi_sdk.exists()
    );

    println!("cargo:warning=BUILD DIAGNOSTIC: About to call maybe_make_cpython");
    maybe_make_cpython(&repo_dir, &wasi_sdk)?;
    println!("cargo:warning=BUILD DIAGNOSTIC: maybe_make_cpython completed successfully");

    let cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: cpython_wasi_dir = {}",
        cpython_wasi_dir.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: cpython_wasi_dir exists: {}",
        cpython_wasi_dir.exists()
    );

    println!("cargo:warning=BUILD DIAGNOSTIC: About to call make_pyo3_config");
    make_pyo3_config(&repo_dir)?;
    println!("cargo:warning=BUILD DIAGNOSTIC: make_pyo3_config completed successfully");

    println!(
        "cargo:warning=BUILD DIAGNOSTIC: NIGHTLY_TOOLCHAIN env var = {:?}",
        env::var("NIGHTLY_TOOLCHAIN")
    );
    let nightly_toolchain = env::var("NIGHTLY_TOOLCHAIN").unwrap_or_else(|_| "nightly".to_string());
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: nightly_toolchain = {}",
        nightly_toolchain
    );

    let runtime_dir = repo_dir.join("runtime");
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: runtime directory = {}",
        runtime_dir.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: runtime directory exists: {}",
        runtime_dir.exists()
    );

    // Try multiple approaches to use nightly Rust, in order of preference:
    // 1. Use rustup run nightly (most reliable for standard environments)
    // 2. If in Nix environment, try to use the nightly cargo directly
    // 3. Fall back to cargo with RUSTUP_TOOLCHAIN
    let (cargo_cmd, use_rustup_run) = if Command::new("rustup")
        .arg("run")
        .arg("nightly")
        .arg("cargo")
        .arg("--version")
        .output()
        .is_ok()
    {
        println!("cargo:warning=BUILD DIAGNOSTIC: Using rustup run nightly");
        ("rustup".to_string(), true)
    } else if let Some(nix_nightly_path) = env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .find(|path| {
            path.contains("nightly") && std::path::Path::new(&format!("{}/cargo", path)).exists()
        })
        .map(|path| format!("{}/cargo", path))
    {
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: Using Nix nightly cargo: {}",
            nix_nightly_path
        );
        (nix_nightly_path, false)
    } else {
        println!("cargo:warning=BUILD DIAGNOSTIC: Using cargo with RUSTUP_TOOLCHAIN fallback");
        ("cargo".to_string(), false)
    };

    let mut cmd = if use_rustup_run {
        let mut c = Command::new("rustup");
        c.current_dir("runtime")
            .arg("run")
            .arg("nightly")
            .arg("cargo")
            .arg("build")
            .arg("-Z")
            .arg("build-std=panic_abort,std")
            .arg("--release")
            .arg("--target=wasm32-wasip1");
        c
    } else {
        let mut c = Command::new(&cargo_cmd);
        c.current_dir("runtime")
            .arg("build")
            .arg("-Z")
            .arg("build-std=panic_abort,std")
            .arg("--release")
            .arg("--target=wasm32-wasip1");
        c
    };

    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Original PATH = {:?}",
        env::var("PATH")
    );

    for (key, _) in env::vars_os() {
        if key
            .to_str()
            .map(|k| (k.starts_with("RUST") || k.starts_with("CARGO")) && k != "RUSTUP_HOME")
            .unwrap_or(false)
        {
            println!(
                "cargo:warning=BUILD DIAGNOSTIC: Removing RUST/CARGO env var: {:?}",
                key.to_str()
            );
            cmd.env_remove(&key);
        }
    }

    cmd.env(
        "RUSTFLAGS",
        "-C relocation-model=pic --cfg pyo3_disable_reference_pool",
    )
    .env("CARGO_TARGET_DIR", out_dir)
    .env("PYO3_CONFIG_FILE", out_dir.join("pyo3-config.txt"));

    // Only set RUSTUP_TOOLCHAIN if not using rustup run (which handles toolchain selection itself)
    if !use_rustup_run {
        cmd.env("RUSTUP_TOOLCHAIN", "nightly");
    }

    println!(
        "cargo:warning=BUILD DIAGNOSTIC: About to run rustup command: {:?}",
        cmd
    );

    // Also try to run rustc directly to see what version it reports
    let rustc_check = if use_rustup_run {
        Command::new("rustup")
            .arg("run")
            .arg("nightly")
            .arg("rustc")
            .arg("--version")
            .output()
    } else if let Some(nix_nightly_path) = env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .find(|path| {
            path.contains("nightly") && std::path::Path::new(&format!("{}/rustc", path)).exists()
        })
        .map(|path| format!("{}/rustc", path))
    {
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: Using Nix rustc: {}",
            nix_nightly_path
        );
        Command::new(&nix_nightly_path).arg("--version").output()
    } else {
        println!("cargo:warning=BUILD DIAGNOSTIC: Using rustc with RUSTUP_TOOLCHAIN=nightly");
        let mut c = Command::new("rustc");
        c.arg("--version").env("RUSTUP_TOOLCHAIN", "nightly");
        c.output()
    };

    if let Ok(output) = rustc_check {
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: rustc version check: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        if !output.status.success() {
            println!(
                "cargo:warning=BUILD DIAGNOSTIC: rustc version check failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    let status = cmd.status().context("Failed to execute rustup command")?;
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: rustup command exit status: {:?}",
        status
    );
    if !status.success() {
        bail!("rustup command failed with status: {:?}", status);
    }
    println!("cargo:rerun-if-changed=runtime");

    let path = out_dir.join("wasm32-wasip1/release/libcomponentize_py_runtime.a");
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Looking for runtime library at: {}",
        path.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Runtime library exists: {}",
        path.exists()
    );

    if path.exists() {
        let clang = wasi_sdk.join(format!("bin/{CLANG_EXECUTABLE}"));
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: Looking for clang at: {}",
            clang.display()
        );
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: Clang exists: {}",
            clang.exists()
        );
        if clang.exists() {
            let name = "libcomponentize_py_runtime.so";

            run(Command::new(clang)
                .arg("-shared")
                .arg("-o")
                .arg(out_dir.join(name))
                .arg("-Wl,--whole-archive")
                .arg(&path)
                .arg("-Wl,--no-whole-archive")
                .arg(format!("-L{}", cpython_wasi_dir.to_str().unwrap()))
                .arg("-lpython3.14"))?;

            compress(out_dir, name, out_dir, false)?;
        } else {
            bail!("no such file: {}", clang.display())
        }
    } else {
        bail!("no such file: {}", path.display())
    }

    let libraries = [
        "libc.so",
        "libwasi-emulated-mman.so",
        "libwasi-emulated-process-clocks.so",
        "libwasi-emulated-getpid.so",
        "libwasi-emulated-signal.so",
        "libc++.so",
        "libc++abi.so",
    ];

    for library in libraries {
        compress(
            &wasi_sdk.join("share/wasi-sysroot/lib/wasm32-wasip2"),
            library,
            out_dir,
            true,
        )?;
    }

    compress(&cpython_wasi_dir, "libpython3.14.so", out_dir, true)?;

    let path = repo_dir.join("cpython/builddir/wasi/install/lib/python3.14");
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Looking for python3.12 lib dir at: {}",
        path.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: python3.12 lib dir exists: {}",
        path.exists()
    );

    if path.exists() {
        let mut builder = Builder::new(Encoder::new(
            File::create(out_dir.join("python-lib.tar.zst"))?,
            ZSTD_COMPRESSION_LEVEL,
        )?);

        add(&mut builder, &path, &path)?;

        builder.into_inner()?.do_finish()?;
    } else {
        bail!("no such directory: {}", path.display())
    }

    let path = repo_dir.join("bundled");
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Looking for bundled dir at: {}",
        path.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: bundled dir exists: {}",
        path.exists()
    );

    if path.exists() {
        let mut builder = Builder::new(Encoder::new(
            File::create(out_dir.join("bundled.tar.zst"))?,
            ZSTD_COMPRESSION_LEVEL,
        )?);

        add(&mut builder, &path, &path)?;

        builder.into_inner()?.do_finish()?;
    } else {
        bail!("no such directory: {}", path.display())
    }

    Ok(())
}

fn compress(src_dir: &Path, name: &str, dst_dir: &Path, rerun_if_changed: bool) -> Result<()> {
    let path = src_dir.join(name);
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: compress called for {} in {} -> {}",
        name,
        src_dir.display(),
        dst_dir.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: compress source path: {}",
        path.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: compress source exists: {}",
        path.exists()
    );

    if rerun_if_changed {
        println!("cargo:rerun-if-changed={}", path.to_str().unwrap());
    }

    if path.exists() {
        let dest_path = dst_dir.join(format!("{name}.zst"));
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: compress destination: {}",
            dest_path.display()
        );

        let mut encoder = Encoder::new(
            File::create(&dest_path).with_context(|| {
                format!("Failed to create destination file: {}", dest_path.display())
            })?,
            ZSTD_COMPRESSION_LEVEL,
        )?;
        io::copy(
            &mut File::open(&path)
                .with_context(|| format!("Failed to open source file: {}", path.display()))?,
            &mut encoder,
        )?;
        encoder.do_finish()?;
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: compress completed successfully for {}",
            name
        );
        Ok(())
    } else {
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: compress FAILED - source file does not exist: {}",
            path.display()
        );
        Err(anyhow!("no such file: {}", path.display()))
    }
}

fn include(path: &Path) -> bool {
    !(matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("a" | "pyc" | "whl")
    ) || matches!(
        path.file_name().and_then(|e| e.to_str()),
        Some("Makefile" | "Changelog" | "NEWS.txt")
    ))
}

fn add(builder: &mut Builder<impl Write>, root: &Path, path: &Path) -> Result<()> {
    println!("cargo:rerun-if-changed={}", path.to_str().unwrap());

    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            add(builder, root, &entry?.path())?;
        }
    } else if include(path) {
        builder.append_file(path.strip_prefix(root)?, &mut File::open(path)?)?;
    }

    Ok(())
}

fn maybe_make_cpython(repo_dir: &Path, wasi_sdk: &Path) -> Result<()> {
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: maybe_make_cpython called with repo_dir={}, wasi_sdk={}",
        repo_dir.display(),
        wasi_sdk.display()
    );
    let cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    if !cpython_wasi_dir.join("libpython3.14.so").exists() {
        fs::create_dir_all(&cpython_wasi_dir)?;
        if !cpython_wasi_dir.join("libpython3.14.a").exists() {
            let cpython_native_dir = repo_dir.join("cpython/builddir/build");
            println!(
                "cargo:warning=BUILD DIAGNOSTIC: cpython_native_dir = {}",
                cpython_native_dir.display()
            );

            let native_python = cpython_native_dir.join(PYTHON_EXECUTABLE);
            println!(
                "cargo:warning=BUILD DIAGNOSTIC: Looking for native python at: {}",
                native_python.display()
            );
            println!(
                "cargo:warning=BUILD DIAGNOSTIC: Native python exists: {}",
                native_python.exists()
            );

            if !native_python.exists() {
                fs::create_dir_all(&cpython_native_dir)?;

                println!("Running configure for native CPython");
                run(Command::new("../../configure")
                    .current_dir(&cpython_native_dir)
                    .arg("--verbose")
                    .arg(format!(
                        "--prefix={}/install",
                        cpython_native_dir.to_str().unwrap()
                    )))?;

                println!("Running make for native CPython");
                run(Command::new("make")
                    .arg("V=1")
                    .current_dir(cpython_native_dir))?;
            }

            let lib_install_dir = cpython_wasi_dir.join("deps");
            build_zlib(wasi_sdk, &lib_install_dir)?;

            let config_guess =
                run(Command::new("../../config.guess").current_dir(&cpython_wasi_dir))?;

            println!("Running configure for wasi CPython");
            run(Command::new("../../Tools/wasm/wasi-env")
                .env(
                    "CONFIG_SITE",
                    "../../Tools/wasm/wasi/config.site-wasm32-wasi",
                )
                .env(
                    "CFLAGS",
                    format!("-fPIC -I{}/deps/include", cpython_wasi_dir.display()),
                )
                .env(
                    "LDFLAGS",
                    format!("-L{}/deps/lib", cpython_wasi_dir.display()),
                )
                .current_dir(&cpython_wasi_dir)
                .args([
                    "../../configure",
                    "-C",
                    "--host=wasm32-unknown-wasip2",
                    &format!("--build={}", String::from_utf8(config_guess)?),
                    &format!(
                        "--with-build-python={}/../build/{PYTHON_EXECUTABLE}",
                        cpython_wasi_dir.to_str().unwrap()
                    ),
                    &format!("--prefix={}/install", cpython_wasi_dir.to_str().unwrap()),
                    "--disable-test-modules",
                    "--enable-ipv6",
                ]))?;

            println!("Running make install for wasi CPython");
            run(Command::new("make")
                .current_dir(&cpython_wasi_dir)
                .arg("install")
                .arg("V=1"))?;
        }

        println!("Creating shared library for wasi CPython");
        run(Command::new(wasi_sdk.join("bin/clang"))
            .arg("--target=wasm32-wasip2")
            .arg("-shared")
            .arg("-o")
            .arg(cpython_wasi_dir.join("libpython3.14.so"))
            .arg("-Wl,--whole-archive")
            .arg(cpython_wasi_dir.join("libpython3.14.a"))
            .arg("-Wl,--no-whole-archive")
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_HMAC.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_BLAKE2.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_MD5.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_SHA1.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_SHA2.a"))
            .arg(cpython_wasi_dir.join("Modules/_hacl/libHacl_Hash_SHA3.a"))
            .arg(cpython_wasi_dir.join("Modules/_decimal/libmpdec/libmpdec.a"))
            .arg(cpython_wasi_dir.join("Modules/expat/libexpat.a"))
            .arg(cpython_wasi_dir.join("deps/lib/libz.a"))
            .arg("-lwasi-emulated-signal")
            .arg("-lwasi-emulated-getpid")
            .arg("-lwasi-emulated-process-clocks")
            .arg("-ldl"))?;
    }

    Ok(())
}

fn run(command: &mut Command) -> Result<Vec<u8>> {
    let command_string = iter::once(command.get_program())
        .chain(command.get_args())
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");

    println!(
        "cargo:warning=BUILD DIAGNOSTIC: About to run command: {}",
        command_string
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Command program: {:?}",
        command.get_program()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Command current_dir: {:?}",
        command.get_current_dir()
    );

    let output = command.output().with_context({
        let command_string = command_string.clone();
        move || format!("Failed to execute command: {}", command_string)
    })?;

    io::stdout().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)?;

    if output.status.success() {
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: Command succeeded: {}",
            command_string
        );
        Ok(output.stdout)
    } else {
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: Command failed with status {:?}: {}",
            output.status, command_string
        );
        println!(
            "cargo:warning=BUILD DIAGNOSTIC: Command stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        bail!(
            "command `{command_string}` failed with status: {:?}",
            output.status
        );
    }
}

fn make_pyo3_config(repo_dir: &Path) -> Result<()> {
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: make_pyo3_config called with repo_dir={}",
        repo_dir.display()
    );
    let out_dir = env::var("OUT_DIR")?;
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: make_pyo3_config OUT_DIR={}",
        out_dir
    );

    let mut cpython_wasi_dir = repo_dir.join("cpython/builddir/wasi");
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: make_pyo3_config cpython_wasi_dir={}",
        cpython_wasi_dir.display()
    );

    let mut cygpath = Command::new("cygpath");
    cygpath.arg("-w").arg(&cpython_wasi_dir);
    if let Ok(output) = cygpath.output() {
        if output.status.success() {
            cpython_wasi_dir = PathBuf::from(String::from_utf8(output.stdout)?.trim());
        } else {
            panic!(
                "cygpath failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    let pyo3_config_source = repo_dir.join("pyo3-config.txt");
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Reading pyo3-config.txt from: {}",
        pyo3_config_source.display()
    );
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: pyo3-config.txt exists: {}",
        pyo3_config_source.exists()
    );

    let mut pyo3_config = fs::read_to_string(&pyo3_config_source).with_context(|| {
        format!(
            "Failed to read pyo3-config.txt from {}",
            pyo3_config_source.display()
        )
    })?;

    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Original pyo3-config.txt content: {}",
        pyo3_config.trim()
    );

    writeln!(
        pyo3_config,
        "lib_dir={}",
        cpython_wasi_dir.to_str().unwrap()
    )?;

    let pyo3_config_dest = Path::new(&out_dir).join("pyo3-config.txt");
    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Writing pyo3-config.txt to: {}",
        pyo3_config_dest.display()
    );

    fs::write(&pyo3_config_dest, &pyo3_config).with_context(|| {
        format!(
            "Failed to write pyo3-config.txt to {}",
            pyo3_config_dest.display()
        )
    })?;

    println!(
        "cargo:warning=BUILD DIAGNOSTIC: Final pyo3-config.txt content: {}",
        pyo3_config.trim()
    );

    println!("cargo:rerun-if-changed=pyo3-config.txt");

    Ok(())
}

fn fetch_extract(url: &str, out_dir: &Path) -> Result<()> {
    let response = reqwest::blocking::get(url)?;
    let decoder = flate2::read::GzDecoder::new(response);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(out_dir)?;
    Ok(())
}

fn add_compile_envs(wasi_sdk: &Path, command: &mut Command) {
    let sysroot = wasi_sdk.join("share/wasi-sysroot");
    let sysroot = sysroot.to_string_lossy();
    command
        .env("AR", wasi_sdk.join("bin/ar"))
        .env("CC", wasi_sdk.join("bin/clang"))
        .env("RANLIB", wasi_sdk.join("bin/ranlib"))
        .env(
            "CFLAGS",
            format!("--target=wasm32-wasi --sysroot={sysroot} -I{sysroot}/include/wasm32-wasip1 -D_WASI_EMULATED_SIGNAL -fPIC"),
        )
        .env(
            "LDFLAGS",
            format!("--target=wasm32-wasip2 --sysroot={sysroot} -L{sysroot}/lib -lwasi-emulated-signal")
        );
}

fn build_zlib(wasi_sdk: &Path, install_dir: &Path) -> Result<()> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    fetch_extract(
        "https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz",
        &out_dir,
    )?;
    let src_dir = out_dir.join("zlib-1.3.1");
    let mut configure = Command::new("./configure");
    add_compile_envs(wasi_sdk, &mut configure);
    configure
        .current_dir(&src_dir)
        .arg("--static")
        .arg(format!("--prefix={}", install_dir.display()));
    run(&mut configure)?;
    let mut make = Command::new("make");
    add_compile_envs(wasi_sdk, &mut make);
    make.current_dir(src_dir)
        .arg(format!("AR={}", wasi_sdk.join("bin/ar").display()))
        .arg("ARFLAGS=rcs")
        .arg(format!("CC={}", wasi_sdk.join("bin/clang").display()))
        .arg("static")
        .arg("install");
    run(&mut make)?;
    Ok(())
}
