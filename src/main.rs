// Copyright 2019 Alex Crawford
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[macro_use]
extern crate log;

use chrono::{DateTime, Utc};
use kube::api::{self, Api, PatchParams, Reflector};
use kube::client::APIClient;
use kube::config;
use log::LevelFilter;
use std::cmp::Ordering;
use structopt::StructOpt;

#[derive(StructOpt)]
struct Options {
    #[structopt(long = "force")]
    /// Forcefully apply available updates
    pub force: bool,

    #[structopt(short = "v", parse(from_occurrences))]
    /// Verbosity level (can be set multiple times)
    pub verbosity: u64,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct ClusterVersionSpec {
    #[serde(rename = "desiredUpdate", default)]
    desired_update: Option<ClusterUpdate>,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct ClusterVersionStatus {
    #[serde(rename = "availableUpdates")]
    available_updates: Option<Vec<ClusterUpdate>>,
    history: Vec<HistoricalEntry>,
}

#[derive(Clone, Debug, Eq, serde::Deserialize, serde::Serialize)]
struct ClusterUpdate {
    force: bool,
    image: String,
    version: semver::Version,
}

impl Ord for ClusterUpdate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.version.cmp(&other.version)
    }
}

impl PartialOrd for ClusterUpdate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for ClusterUpdate {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct HistoricalEntry {
    #[serde(rename = "completionTime")]
    completion_time: Option<DateTime<Utc>>,
}

type ClusterVersion = api::Object<ClusterVersionSpec, ClusterVersionStatus>;

fn main() -> Result<(), kube::Error> {
    let options = Options::from_args();

    env_logger::Builder::from_default_env()
        .filter(
            Some(module_path!()),
            match options.verbosity {
                0 => LevelFilter::Warn,
                1 => LevelFilter::Info,
                2 => LevelFilter::Debug,
                _ => LevelFilter::Trace,
            },
        )
        .init();

    let client = Api::<ClusterVersion>::customResource(
        APIClient::new(config::load_kube_config()?),
        "clusterversions",
    )
    .group("config.openshift.io")
    .version("v1");

    let reflector = Reflector::new(client.clone())
        .fields("metadata.name==version")
        .init()?;
    loop {
        if let Err(error) = reflector.poll() {
            error!("Failed to poll reflector: {}", error);
        }

        match reflector.read() {
            Ok(mut versions) => {
                let version = match versions.pop() {
                    Some(version) => version,
                    None => {
                        error!("Unable to find ClusterVersion");
                        continue;
                    }
                };

                if let Some(status) = &version.status {
                    if let Some(latest) = status.history.first() {
                        if latest.completion_time.is_none() {
                            debug!("Waiting for update to complete...");
                            continue;
                        }
                    }
                }

                if let Err(error) = apply_available_update(&client, &options, version) {
                    error!("Failed to apply update: {}", error)
                }
            }
            Err(error) => error!("Failed to read ClusterVersion: {}", error),
        }
    }
}

fn apply_available_update(
    client: &Api<ClusterVersion>,
    options: &Options,
    version: ClusterVersion,
) -> Result<(), kube::Error> {
    trace!("{:?}", version.status);

    let update = match version.status.and_then(|status| status.available_updates) {
        Some(updates) => updates.into_iter().max(),
        None => return Ok(()),
    };

    if let Some(mut update) = update {
        update.force = options.force;
        info!("Attempting to update to {}", update.version);
        client.patch(
            "version",
            &PatchParams::default(),
            serde_json::to_vec(&ClusterVersion {
                types: version.types,
                metadata: version.metadata,
                spec: ClusterVersionSpec {
                    desired_update: Some(update),
                },
                status: None,
            })
            .expect("Serialize to JSON"),
        )?;
    }

    Ok(())
}
