use anyhow::Result;
use aws_config::BehaviorVersion;
pub use aws_sdk_ssm::Client;

use futures::stream::{self, Stream};

pub async fn client() -> Client {
  let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
  Client::new(&config)
}

pub fn all_parameters_by_path(client: &Client, prefix: &str) -> impl Stream<Item = Result<Vec<aws_sdk_ssm::types::Parameter>>> {
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
