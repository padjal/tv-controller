use anyhow::{Context, Result};
use std::{fs, path::Path};
use uuid::Uuid;

pub struct Config {
    pub server_url: String,
    pub device_name: String,
    pub device_id: Uuid,
    pub agent_port: u16,
}

impl Config {
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();
        let server_url = std::env::var("SERVER_URL").context("SERVER_URL not set")?;
        let device_name = std::env::var("DEVICE_NAME").context("DEVICE_NAME not set")?;
        let agent_port = std::env::var("AGENT_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .context("AGENT_PORT must be a valid port number")?;
        let device_id = load_or_generate_id()?;
        Ok(Self { server_url, device_name, device_id, agent_port })
    }
}

fn load_or_generate_id() -> Result<Uuid> {
    let path = Path::new("/etc/tv-agent/device.id");
    if path.exists() {
        let s = fs::read_to_string(path).context("failed to read /etc/tv-agent/device.id")?;
        return s.trim().parse().context("device.id contains an invalid UUID");
    }
    let id = Uuid::new_v4();
    fs::create_dir_all("/etc/tv-agent").context("failed to create /etc/tv-agent")?;
    fs::write(path, id.to_string()).context("failed to write /etc/tv-agent/device.id")?;
    Ok(id)
}
