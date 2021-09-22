use std::{net::IpAddr, path::PathBuf};

use anyhow::Result;
use reqwest::Method;
use rusqlite::Connection;
use serde::Deserialize;
use structopt::StructOpt;

/// Incrementally synchronize Wikipedia's block list to a local SQLite3 database.
#[derive(StructOpt, Debug)]
struct Opt {
  #[structopt(long)]
  db: PathBuf,
}

#[derive(Deserialize)]
struct WpApiRes {
  #[serde(rename = "continue")]
  _continue: Option<WpContinue>,
  query: WpQuery,
}

#[derive(Deserialize)]
struct WpContinue {
  bkcontinue: String,
}

#[derive(Deserialize)]
struct WpQuery {
  blocks: Vec<WpBlock>,
}

#[derive(Deserialize)]
struct WpBlock {
  id: u64,
  timestamp: String,
  expiry: String,
  rangestart: Option<IpAddr>,
  rangeend: Option<IpAddr>,
}

#[tokio::main]
async fn main() -> Result<()> {
  if std::env::var("RUST_LOG").is_err() {
    std::env::set_var("RUST_LOG", "info");
  }

  pretty_env_logger::init_timed();
  let opt = Opt::from_args();

  let conn = Connection::open(&opt.db)?;
  conn.execute_batch(
    r#"
  pragma journal_mode = wal;
  create table if not exists wpbl (
    `id` integer not null primary key,
    `timestamp` text not null,
    `expiry` text not null,
    `rangestart` text not null,
    `rangeend` text not null
  );
  create index if not exists `by_timestamp` on wpbl (`timestamp`);
  create index if not exists `by_expiry` on wpbl (`expiry`);
  create index if not exists `by_rangestart` on wpbl (`rangestart`);
  "#,
  )?;

  let max_ts: Option<String> =
    conn.query_row("select max(`timestamp`) from wpbl", [], |row| row.get(0))?;
  let max_ts = max_ts.unwrap_or_else(|| "1970-01-01T00:00:00Z".into());
  log::info!("Starting at timestamp {}.", max_ts);

  let mut bkcontinue: Option<String> = None;
  let client = reqwest::Client::new();
  loop {
    let mut params: Vec<(String, String)> = vec![
      ("action".into(), "query".into()),
      ("format".into(), "json".into()),
      ("list".into(), "blocks".into()),
      ("bkdir".into(), "newer".into()),
      ("bklimit".into(), "max".into()),
      ("bkprop".into(), "id|timestamp|expiry|range".into()),
      ("bkstart".into(), max_ts.clone()),
    ];
    if let Some(x) = bkcontinue.clone() {
      params.push(("bkcontinue".into(), x));
    }

    let req = client
      .request(Method::GET, "https://en.wikipedia.org/w/api.php")
      .query(&params)
      .build()?;
    let res = client.execute(req).await?.text().await?;
    log::debug!("{}", res);
    let res: WpApiRes = serde_json::from_str(&res)?;
    let mut count = 0usize;
    for block in res.query.blocks {
      if let (Some(rangestart), Some(rangeend)) = (block.rangestart, block.rangeend) {
        let rangestart = encode_ipaddr(rangestart);
        let rangeend = encode_ipaddr(rangeend);
        if rangestart == "00000000" {
          continue;
        }
        conn.execute("insert or ignore into wpbl (`id`, `timestamp`, `expiry`, `rangestart`, `rangeend`) values(?, ?, ?, ?, ?)", rusqlite::params![
          block.id,
          block.timestamp,
          block.expiry,
          rangestart,
          rangeend,
        ])?;
        count += 1;
      }
    }
    log::info!(
      "Got {} block entries from continuation {:?}.",
      count,
      bkcontinue
    );

    match res._continue {
      Some(x) => {
        bkcontinue = Some(x.bkcontinue);
      }
      None => {
        log::info!("Done.");
        return Ok(());
      }
    }
  }
}

fn encode_ipaddr(addr: IpAddr) -> String {
  match addr {
    IpAddr::V4(x) => hex::encode_upper(&x.octets()),
    IpAddr::V6(x) => hex::encode_upper(&x.octets()),
  }
}
