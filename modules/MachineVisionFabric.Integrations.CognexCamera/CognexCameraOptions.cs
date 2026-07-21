namespace MachineVisionFabric.Integrations.CognexCamera;

/// <summary>
/// Configuration for the Cognex In-Sight HMI camera source.
///
/// ┌─────────────────────────────────────────────────────────────────┐
/// │  CAMERA IP: Change IpAddress below to match your camera.       │
/// │  Default: 192.168.1.11                                          │
/// │                                                                 │
/// │  In pipeline.json → node config:                                │
/// │    "config": { "ipAddress": "192.168.1.XX", ... }              │
/// └─────────────────────────────────────────────────────────────────┘
/// </summary>
public sealed class CognexCameraOptions
{
    /// <summary>Camera IP address. Change this to match your camera.</summary>
    public string IpAddress { get; set; } = "192.168.1.11";

    /// <summary>Logical camera identifier used in frame metadata.</summary>
    public string CameraId { get; set; } = "cognex-cam-1";

    /// <summary>HMI WebSocket port (default: 8087).</summary>
    public int HmiPort { get; set; } = 8087;

    /// <summary>HMI WebSocket path (default: /ws).</summary>
    public string HmiWebSocketPath { get; set; } = "/ws";

    /// <summary>Use TLS for HMI connection (default: false).</summary>
    public bool HmiUseTls { get; set; } = false;

    /// <summary>HMI login username (default: admin). Sent Base64-encoded.</summary>
    public string HmiUsername { get; set; } = "admin";

    /// <summary>HMI login password (default: empty). Sent Base64-encoded.</summary>
    public string HmiPassword { get; set; } = "";

    /// <summary>
    /// How to acquire frames:
    ///   passive-listen   — subscribe to resultChanged events (camera triggers externally)
    ///   manual-trigger   — send software trigger periodically
    /// </summary>
    public string AcquisitionMode { get; set; } = "passive-listen";

    /// <summary>Interval between software triggers in manual-trigger mode (ms).</summary>
    public int ManualTriggerIntervalMs { get; set; } = 500;

    /// <summary>Interval between ready-signal sends in passive mode (ms).</summary>
    public int HmiReadyIntervalMs { get; set; } = 1000;

    /// <summary>Response timeout for HMI requests (ms).</summary>
    public int ResponseTimeoutMs { get; set; } = 1500;

    /// <summary>Internal frame queue capacity.</summary>
    public int BoundedCapacity { get; set; } = 8;

    /// <summary>Delay before connecting on startup (ms). 0 = no delay.</summary>
    public int StartupDelayMs { get; set; } = 0;

    /// <summary>Reopen HMI session before each manual trigger (helps with stale sessions).</summary>
    public bool ReopenSessionBeforeManualTrigger { get; set; } = true;

    /// <summary>Number of trigger retries on failure.</summary>
    public int ManualTriggerRetryCount { get; set; } = 3;

    /// <summary>Optional HMI image query string.</summary>
    public string HmiImageQuery { get; set; } = string.Empty;

    /// <summary>Log detailed diagnostics to console.</summary>
    public bool LogDiagnostics { get; set; } = true;
}
