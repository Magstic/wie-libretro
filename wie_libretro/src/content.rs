use std::{collections::BTreeMap, sync::Arc};

use wie_backend::{Emulator, Options, Platform, extract_zip};
use wie_j2me::J2MEEmulator;
use wie_ktf::KtfEmulator;
use wie_lgt::LgtEmulator;
use wie_skt::SktEmulator;
use wie_util::{Result, WieError};

use crate::environment::RuntimeOption;

#[derive(Clone)]
pub struct LoadedContent {
    pub name: String,
    pub data: Arc<Vec<u8>>,
}

impl LoadedContent {
    pub fn single(name: String, data: Vec<u8>) -> Self {
        Self { name, data: Arc::new(data) }
    }
}

pub fn load_emulator(platform: Box<dyn Platform>, content: &LoadedContent, runtime: RuntimeOption) -> Result<Box<dyn Emulator + Send>> {
    tracing::info!("load_emulator: data_len={}, runtime={:?}", content.data.len(), runtime);

    let files = extract_zip(content.data.as_slice()).map_err(|err| {
        tracing::error!("load_emulator: extract_zip failed: {err}");
        WieError::FatalError(format!("Content is not a valid ZIP archive: {err}"))
    })?;
    tracing::info!("load_emulator: zip extracted, {} files", files.len());

    match runtime {
        RuntimeOption::Ktf => {
            if !KtfEmulator::loadable_archive(&files) {
                return Err(WieError::FatalError("Content is not a KTF archive".into()));
            }
            Ok(Box::new(KtfEmulator::from_archive(platform, files, emulator_options())?))
        }
        RuntimeOption::Lgt => {
            if !LgtEmulator::loadable_archive(&files) {
                return Err(WieError::FatalError("Content is not an LGT archive".into()));
            }
            Ok(Box::new(LgtEmulator::from_archive(platform, files, emulator_options())?))
        }
        RuntimeOption::Skt => {
            if !SktEmulator::loadable_archive(&files) {
                return Err(WieError::FatalError("Content is not an SKT archive".into()));
            }
            Ok(Box::new(SktEmulator::from_archive(platform, files)?))
        }
        RuntimeOption::J2me => {
            let jar_filename = find_jar(&files)?;
            let jar = files.get(&jar_filename).unwrap().clone();
            Ok(Box::new(J2MEEmulator::from_jar(platform, &jar_filename, jar)?))
        }
        RuntimeOption::Auto => {
            if KtfEmulator::loadable_archive(&files) {
                Ok(Box::new(KtfEmulator::from_archive(platform, files, emulator_options())?))
            } else if LgtEmulator::loadable_archive(&files) {
                Ok(Box::new(LgtEmulator::from_archive(platform, files, emulator_options())?))
            } else if SktEmulator::loadable_archive(&files) {
                Ok(Box::new(SktEmulator::from_archive(platform, files)?))
            } else {
                let jar_filename = find_jar(&files)?;
                let jar = files.get(&jar_filename).unwrap().clone();
                Ok(Box::new(J2MEEmulator::from_jar(platform, &jar_filename, jar)?))
            }
        }
    }
}

fn emulator_options() -> Options {
    Options {
        enable_gdbserver: false,
        profile: None,
    }
}

fn find_jar(files: &BTreeMap<String, Vec<u8>>) -> Result<String> {
    files
        .keys()
        .find(|name| name.ends_with(".jar"))
        .cloned()
        .ok_or_else(|| WieError::FatalError("No JAR file found in ZIP archive".into()))
}
