/// One process row sampled from the remote `ps` (#23). CPU/mem are percentages
/// as reported by `ps` (pcpu/pmem); `command` is the (width-truncated) args.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub user: String,
    pub cpu: f32,
    pub mem: f32,
    pub command: String,
}

/// One GPU sample from the dedicated `nvidia-smi` channel.
///
/// `utilization` is a 0.0..1.0 fraction; `None` means the driver reported
/// `[N/A]` (common on vGPU / MIG setups) rather than 0%. Memory is MiB.
#[derive(Debug, Clone, Default)]
pub struct GpuSample {
    pub index: u32,
    pub name: String,
    pub utilization: Option<f32>,
    pub mem_used_mib: Option<u64>,
    pub mem_total_mib: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct SystemDetails {
    pub overview: Vec<(String, String)>,
    pub cpu_info: Vec<(String, String)>,
    pub gpu_info: Vec<(String, String)>,
    pub cpu_usage: Vec<(String, String)>,
    pub memory: Vec<(String, String)>,
    pub swap: Vec<(String, String)>,
    pub networks: Vec<(String, String, String, String, String)>,
    pub filesystems: Vec<(String, String, String, String, String)>,
}

/// One SSH tunnel row shown in the runtime tunnel panel (#206).
#[derive(Debug, Clone)]
pub struct RuntimeTunnelInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub bind_addr: String,
    pub bind_port: u16,
    pub host: String,
    pub host_port: u16,
    pub active: bool,
    pub status: String,
}
