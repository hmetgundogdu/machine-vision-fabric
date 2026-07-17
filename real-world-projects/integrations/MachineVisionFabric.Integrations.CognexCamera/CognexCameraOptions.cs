namespace MachineVisionFabric.Integrations.CognexCamera;

public sealed class CognexCameraOptions
{
    public string IpAddress { get; set; } = "192.168.1.11";

    public string CameraId { get; set; } = "cognex-cam-1";

    public string Username { get; set; } = "admin";

    public string Password { get; set; } = string.Empty;

    public int HmiPort { get; set; } = 8087;

    public string HmiWebSocketPath { get; set; } = "/ws";

    public bool HmiUseTls { get; set; }

    public int ResponseTimeoutMs { get; set; } = 1500;

    public int HmiReadyIntervalMs { get; set; } = 1000;

    public int BoundedCapacity { get; set; } = 8;

    public int? MaxFrames { get; set; } = 6;

    public string AcquisitionMode { get; set; } = "passive-listen";

    public int ManualTriggerIntervalMs { get; set; } = 500;

    public bool ReopenSessionBeforeManualTrigger { get; set; } = true;

    public int ManualTriggerRetryCount { get; set; } = 3;

    public int StartupDelayMs { get; set; } = 0;

    public string HmiImageQuery { get; set; } = string.Empty;

    public bool LogDiagnostics { get; set; } = true;
}
