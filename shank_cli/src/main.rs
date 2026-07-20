use std::{fs::File, io::Write};

use anyhow::{anyhow, Result};
use env_logger::Env;
use log::{debug, info};
use shank_idl::{parse_file, ParseIdlConfig, ParseIdlOpts};

struct IdlConfiguration {
    program_id: String,
    name: &'static str,
    paths: Vec<&'static str>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let crate_root = std::env::current_dir()?;

    let envs = envfile::EnvFile::new(crate_root.join(".cargo").join("programs.env"))?;
    let ncn_program_id = envs
        .get("NCN_PROGRAM_ID")
        .ok_or_else(|| anyhow!("NCN_PROGRAM_ID not found"))?
        .to_string();

    let idl_configs = vec![IdlConfiguration {
        program_id: ncn_program_id,
        name: "ncn_program",
        paths: vec!["core", "program"],
    }];

    let crate_root = std::env::current_dir().unwrap();
    let out_dir = crate_root.join("idl");
    for idl in idl_configs {
        let mut idls = Vec::new();
        for path in idl.paths {
            let cargo_toml = crate_root.join(path).join("Cargo.toml");
            if !cargo_toml.exists() {
                return Err(anyhow!(
                    "Did not find Cargo.toml at the path: {}",
                    crate_root.display()
                ));
            }
            // NOTE: shank_idl's `extract_idl` discovers the crate manifest via
            // the `cargo_toml` crate, which cannot parse `edition = "2024"`
            // workspace manifests. Every program crate here uses the
            // conventional `src/lib.rs` lib path and the workspace version, so
            // call `parse_file` directly instead of going through the manifest.
            let lib_full_path_str = crate_root.join(path).join("src").join("lib.rs");
            let lib_full_path = lib_full_path_str
                .to_str()
                .ok_or_else(|| anyhow!("Invalid Path"))?;
            debug!("lib_full_path: {:?}", lib_full_path);
            // Extract IDL and convert to JSON
            let opts = ParseIdlOpts {
                program_address_override: Some(idl.program_id.to_string()),
                ..ParseIdlOpts::default()
            };
            let parse_config = ParseIdlConfig {
                program_version: env!("CARGO_PKG_VERSION").to_string(),
                program_name: idl.name.to_string(),
                detect_custom_struct: opts.detect_custom_struct,
                require_program_address: opts.require_program_address,
                program_address_override: opts.program_address_override,
            };
            let idl = parse_file(lib_full_path, &parse_config)?
                .ok_or_else(|| anyhow!("No IDL could be extracted"))?;
            idls.push(idl);
        }

        let mut accumulator = idls.pop().unwrap();
        for other_idls in idls {
            accumulator.constants.extend(other_idls.constants);
            accumulator.instructions.extend(other_idls.instructions);
            accumulator.accounts.extend(other_idls.accounts);
            accumulator.types.extend(other_idls.types);
            if let Some(events) = other_idls.events {
                if let Some(accumulator_events) = &mut accumulator.events {
                    accumulator_events.extend(events);
                } else {
                    accumulator.events = Some(events);
                }
            }
            if let Some(errors) = other_idls.errors {
                if let Some(accumulator_errors) = &mut accumulator.errors {
                    accumulator_errors.extend(errors);
                } else {
                    accumulator.errors = Some(errors);
                }
            }
        }
        accumulator.name = idl.name.to_string();

        let idl_json = accumulator.try_into_json()?;
        let mut idl_path = out_dir.join(idl.name);
        idl_path.set_extension("json");

        info!("Writing IDL to {:?}", idl_path);
        let mut idl_json_file = File::create(idl_path)?;
        idl_json_file.write_all(idl_json.as_bytes())?;
    }

    Ok(())
}
