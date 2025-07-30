use std::{
  collections::HashMap,
  fs,
  path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use aws_config::BehaviorVersion;
use aws_sdk_ssm::{Client, types::ParameterType};
use clap::{Parser, Subcommand};
use tokio::fs as tokio_fs;
use walkdir::WalkDir;

const CHUNK_SIZE: usize = 4096;

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
  #[command(subcommand)]
  command: Command,
}

#[derive(Subcommand)]
enum Command {
  Upload {
    #[arg(long)]
    dir: PathBuf,

    #[arg(long)]
    prefix: String,
  },
  Download {
    #[arg(long, conflicts_with("name"), required_unless_present("name"))]
    prefix: Option<String>,
    #[arg(long, conflicts_with("prefix"), required_unless_present("prefix"))]
    name: Option<String>,

    #[arg(long)]
    dir: PathBuf,
  },
  Copy {
    #[arg(long)]
    prefix: String,
    #[arg(long)]
    to_prefix: String,
  },
  Env {
    #[arg(long, short, env)]
    file: String,
    #[arg(long, short, env)]
    base: String,
    #[arg(long, short, env, value_delimiter = ',')]
    vars: Vec<String>,
  },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let cli = Cli::parse();
  let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
  let client = Client::new(&config);

  match cli.command {
    Command::Upload { dir, prefix } => upload_dir(&client, dir, prefix).await?,
    Command::Download { prefix, dir, name } => download_to_dir(&client, prefix, name, dir).await?,
    Command::Env{ file, base, vars } => set_env(&client, file, base, vars).await?,
    Command::Copy { prefix, to_prefix } => copy(&client, prefix, to_prefix).await?,
  }

  Ok(())
}

async fn upload_dir(client: &Client, dir: PathBuf, prefix: String) -> anyhow::Result<()> {
  for entry in WalkDir::new(&dir).into_iter().filter_map(Result::ok).filter(|e| e.file_type().is_file()) {
    let rel_path = entry.path().strip_prefix(&dir)?;
    let content = tokio_fs::read(entry.path()).await?;

    let param_base = format!("{}{}", prefix.trim_end_matches('/'), to_ssm_key(rel_path));

    if content.len() > CHUNK_SIZE {
      for (i, chunk) in content.chunks(CHUNK_SIZE).enumerate() {
        let key = format!("{}.part{}", param_base, i);
        client
          .put_parameter()
          .name(&key)
          .value(String::from_utf8_lossy(chunk))
          .overwrite(true)
          .r#type(ParameterType::SecureString)
          .send()
          .await?;
      }
    } else {
      client
        .put_parameter()
        .name(&param_base)
        .value(String::from_utf8_lossy(&content))
        .overwrite(true)
        .r#type(ParameterType::SecureString)
        .send()
        .await?;
    }
  }
  Ok(())
}

fn all_parameters_by_path(client: &Client, prefix: &str) -> impl futures::stream::Stream<Item = Result<Vec<aws_sdk_ssm::types::Parameter>>> {
  stream::try_unfold((true, None), move |(first, next_token)| async move {
    if first || next_token.is_some() {
      let resp = client
        .get_parameters_by_path()
        .with_decryption(true)
        .path(prefix)
        .set_next_token(next_token)
        .recursive(true)
        .send()
        .await?;
      Ok(Some((resp.parameters().to_vec(), (false, resp.next_token().map(|s| s.to_string())))))
    } else {
      Ok(None)
    }
  })
}
use futures::stream::{self, TryStreamExt};
async fn download_to_dir(client: &Client, prefix: Option<String>, name: Option<String>, output_dir: PathBuf) -> anyhow::Result<()> {
  let parameters = match (prefix, name) {
    (Some(prefix), _) => { 
      let params = all_parameters_by_path(client, &prefix).try_collect::<Vec<_>>().await?.into_iter().flatten();

      let mut parameters: HashMap<String, Vec<(usize, String)>> = HashMap::new();
      for param in params {
        let name = param.name().unwrap().to_string();
        let rel_path = name.trim_start_matches(&format!("{prefix}/"));
        let content = param.value().unwrap().to_string();

        if let Some((base, part)) = rel_path.rsplit_once(".part") {
          let idx: usize = part.parse()?;
          parameters.entry(base.to_string()).or_default().push((idx, content));
        } else {
          parameters.entry(rel_path.to_string()).or_default().push((0, content));
        }
      }
      parameters
    },
    (_, Some(name)) => {
      let resp = client.get_parameter().name(name).with_decryption(true).send().await?;
      resp.parameter().into_iter().map(|p| (p.name().unwrap().rsplit('/').nth(0).unwrap().to_string(), p.value().into_iter().map(|v|(0, v.to_string())).collect())).collect()
    },
    _ => { [].into() }
  };

  for (rel_path, mut chunks) in parameters {
    chunks.sort_by_key(|(i, _)| *i);
    let content: String = chunks.into_iter().map(|(_, c)| c).collect();

    let full_path = output_dir.join(rel_path);
    if let Some(parent) = full_path.parent() {
      fs::create_dir_all(parent)?;
    }
    fs::write(full_path, content)?;
  }

  Ok(())
}

fn to_ssm_key(path: &Path) -> String {
  let mut key = String::new();
  for comp in path.components() {
    key.push('/');
    key.push_str(&comp.as_os_str().to_string_lossy());
  }
  key
}

pub async fn set_env(client: &Client, file: String, base: String, vars: Vec<String>) -> Result<()> {
  println!("Getting vars {vars:?} from {base}");
  let resp = client
    .get_parameters()
    .set_names(Some(vars.iter().map(|v| format!("{base}/{v}")).collect()))
    .with_decryption(true)
    .send()
    .await
    .context("Failed to fetch parameters from SSM")?;

  let output = resp.parameters().iter().map(|p| {
    let name = p.name().unwrap_or_default();
    let value = p.value().unwrap_or_default();

    let key = name.rsplit('/').next().unwrap_or(&name).to_ascii_uppercase();

    format!("{key}=\"{value}\"")
  }).collect::<Vec<_>>().join("\n");

  println!("Writing to file {file}");
  fs::write(&file, output).context(format!("Failed to write to {file}"))?;

  Ok(())
}

pub async fn copy(client: &Client, prefix: String, to_prefix: String) -> Result<()> {
  let params = all_parameters_by_path(client, &prefix).try_collect::<Vec<_>>().await?;

  for param in params.into_iter().flatten() {
    let name = param.name().unwrap();
    let value = param.value().unwrap();

    let new_name = format!("{}{}", to_prefix, name.trim_start_matches(&prefix));

    client
      .put_parameter()
      .name(new_name)
      .value(value)
      .overwrite(true)
      .r#type(param.r#type().unwrap().clone())
      .send()
      .await?;
  }

  Ok(())
}
