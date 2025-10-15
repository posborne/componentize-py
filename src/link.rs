use anyhow::Result;

use crate::Library;

pub fn link_libraries(libraries: &[Library], adapter: &str) -> Result<Vec<u8>> {
    let mut linker = wit_component::Linker::default()
        .validate(true)
        .use_built_in_libdl(true);

    for Library {
        name,
        module,
        dl_openable,
    } in libraries
    {
        linker = linker.library(name, module, *dl_openable)?;
    }

    let adapter_module = match adapter {
        "proxy" => wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_PROXY_ADAPTER,
        "reactor" => {
            wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_REACTOR_ADAPTER
        }
        _ => panic!("Adapater {adapter} not supported"),
    };
    linker = linker.adapter("wasi_snapshot_preview1", adapter_module)?;

    linker.encode().map_err(|e| anyhow::anyhow!(e))
}
