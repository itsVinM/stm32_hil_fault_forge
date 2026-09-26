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
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ReportParams {
    #[serde(default)]
    pub quiet: bool,
}

fn default_seed() -> u32 { 42 }
fn default_packets() -> u16 { 1500 }
fn default_cadence() -> u16 { 5 }
fn default_profile() -> String { "fuzz".into() }
fn default_port() -> String { "auto".into() }

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
            Some(w)
        } else {
            None
        };

        crate::Opts {
            simulated: self.host.port == "simulate",
            seed: self.campaign.seed,
            packets: self.campaign.packets,
            cadence_ms: self.campaign.cadence_ms,
            profile: Box::leak(self.campaign.profile.clone().into_boxed_str()),
            quiet: self.report.quiet,
            weights,
        }
    }
}