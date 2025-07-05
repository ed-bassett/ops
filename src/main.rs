use std::{
  collections::HashMap,
  fs,
  path::{Path, PathBuf},
};

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
    #[arg(long)]
    prefix: String,

    #[arg(long)]
    dir: PathBuf,
  },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let cli = Cli::parse();
  let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
  let client = Client::new(&config);

  match cli.command {
    Command::Upload { dir, prefix } => upload_dir(&client, dir, prefix).await?,
    Command::Download { prefix, dir } => download_to_dir(&client, prefix, dir).await?,
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

async fn download_to_dir(client: &Client, prefix: String, output_dir: PathBuf) -> anyhow::Result<()> {
  let mut next_token = None;
  let mut parameters: HashMap<String, Vec<(usize, String)>> = HashMap::new();

  loop {
    let resp = client
      .get_parameters_by_path()
      .with_decryption(true)
      .path(&prefix)
      .set_next_token(next_token)
      .recursive(true)
      .send()
      .await?;

    for param in resp.parameters() {
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

    if let Some(token) = resp.next_token() {
      next_token = Some(token.to_string());
    } else {
      break;
    }
  }

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
