use std::{collections::HashMap, io::Write};

use anyhow::Result;
use futures::{future::try_join_all, TryStreamExt};
use serde::{Serialize, Deserialize};
use itertools::Itertools;
use tempfile::NamedTempFile;

use crate::ssm;

#[derive(Debug, Serialize, Deserialize)]
pub struct ComposeFile {
  pub services: HashMap<String, Service>,
  pub secrets: Option<HashMap<String, SecretDefinition>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Service {
  pub secrets: Option<Vec<ServiceSecret>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServiceSecret {
  NameOnly(String),
  Detailed(ServiceSecretDetail),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServiceSecretDetail {
  pub source: String,
  pub target: Option<String>,
  pub uid: Option<String>,
  pub gid: Option<String>,
  pub mode: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged, rename_all = "lowercase")]
pub enum SecretDefinition {
  File { file: String },
  Environment { environment: String },
  External { external: Option<bool> },
}

fn parse(path: &str) -> Result<ComposeFile> {
  let yaml = std::fs::read_to_string(path)?;
  let compose: ComposeFile = serde_yaml::from_str(&yaml)?;
  Ok(compose)
}

pub async fn exec_compose(client: &ssm::Client, path: &str, namespace: &str, args: Vec<String>) -> Result<()> {
  let compose = parse(path)?;

  let secret_names = compose
    .services
    .into_iter()
    .flat_map(|(service_name, service)|
      service.secrets.unwrap_or(vec![]).into_iter().map(move |secret| {
        let secret_name = match secret {
          ServiceSecret::NameOnly(name) => name,
          ServiceSecret::Detailed(detail) => detail.source,
        };

        format!("/apps/{namespace}/{service_name}/secrets/{secret_name}")
      })
    )
    .collect::<Vec<_>>();

  let paths = secret_names.iter().into_group_map_by(|n|n.rsplit_once('/').map(|(p, _)|p).unwrap_or(n).to_owned());

  let path_secrets = try_join_all(
    paths
      .keys()
      .map(|p| async move {
        dbg!(&p);
        let params = ssm::all_parameters_by_path(client, p).try_collect::<Vec<_>>().await?.into_iter().flatten().collect::<Vec<_>>();
        anyhow::Ok(params.into_iter().map(|p| (p.name().expect("missing name").to_string(), p.value().unwrap_or("").to_string())).collect::<Vec<_>>())
      })
  )
    .await?.into_iter().flatten().collect::<HashMap<_,_>>();

  let secrets = ComposeFile{
    services: [].into(),
    secrets: Some(
      secret_names
        .iter()
        .map(|name| {
          let secret_name = name.rsplit_once("/").map(|(_, name)|name).unwrap_or(name).to_owned();
          let environment = name.replace('/', "_").to_uppercase();
          (secret_name, SecretDefinition::Environment { environment })
        })
        .collect(),
    ),
  };

  let envs = path_secrets.iter().map(|(name, value)| {
    let env_name = name.replace('/', "_").to_uppercase();
    (env_name, value.clone())
  }).collect::<Vec<_>>();

  println!("{}", serde_yaml::to_string(&secrets)?);
  let compose_file = write_compose_to_temp_file(&secrets)?;
  dbg!(&compose_file.path());

  std::process::Command::new("docker")
    .envs(envs)
    .arg("compose")
    .arg("-f")
    .arg(path)
    .arg("-f")
    .arg(compose_file.path())
    .args(args)
    .status()?;

  Ok(())
}

fn write_compose_to_temp_file(compose: &ComposeFile) -> Result<NamedTempFile> {
  let mut file = NamedTempFile::new()?;

  serde_yaml::to_writer(&file, compose)?;
  file.flush()?;

  Ok(file)
}