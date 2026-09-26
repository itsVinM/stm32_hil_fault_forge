//! Campaign configuration loaded from YAML.

use faultforge_shared::N_FAULT_KINDS;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct CampaignConfigYaml {
    pub campaign: CampaignParams,
    #[serde(default)]
    pub host: HostParams,
    #[serde(default)]
    pub report: ReportParams,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CampaignParams {
    #[serde(default = "default_seed")]
    pub seed: u32,
    #[serde(default = "default_packets")]
    pub packets: u16,
    #[serde(default = "default_cadence")]
    pub cadence_ms: u16,
    #[serde(default = "default_profile")]
    pub profile: String,
    #[serde(default)]
    pub weights: Vec<u8>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct HostParams {
    #[serde(default = "default_port")]
    pub port: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ReportParams {
    #[serde(default = "default_csv_dir")]
    pub csv_dir: String,
    #[serde(default = "default_csv_prefix")]
    pub csv_prefix: String,
    #[serde(default)]
    pub quiet: bool,
}

fn default_seed() -> u32 { 42 }
fn default_packets() -> u16 { 1500 }
fn default_cadence() -> u16 { 5 }
fn default_profile() -> String { "fuzz".into() }
fn default_port() -> String { "auto".into() }
fn default_baud() -> u32 { 115_200 }
fn default_timeout() -> u64 { 50 }
fn default_csv_dir() -> String { "faultforge_out".into() }
fn default_csv_prefix() -> String { "campaign".into() }

impl CampaignConfigYaml {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let cfg: Self = serde_yaml::from_str(&content)?;
        Ok(cfg)
    }

    pub fn to_opts(&self) -> crate::Opts {
        let weights = if self.campaign.weights.len() == N_FAULT_KINDS {
            let mut w = [0u8; N_FAULT_KINDS];
            w.copy_from_slice(&self.campaign.weights);
            w
        } else {
            crate::campaign::profile_weights(&self.campaign.profile)
        };

        crate::Opts {
            simulated: self.host.port == "simulate",
            seed: self.campaign.seed,
            packets: self.campaign.packets,
            cadence_ms: self.campaign.cadence_ms,
            profile: Box::leak(self.campaign.profile.clone().into_boxed_str()),
            quiet: self.report.quiet,
        }
    }
}