namespace Mvf.Graph.Control;

public sealed class SimulatedPlcGateOptions
{
    public bool Enabled { get; set; } = true;

    public bool ProductPresent { get; set; } = true;

    public int DelayBeforePresentMs { get; set; }

    public IReadOnlyList<bool> ProductPresentSequence { get; set; } = [];

    public bool HoldLastSequenceValue { get; set; } = true;

    public string SourceName { get; set; } = "simulated-plc";

    public string StationId { get; set; } = "station-1";

    public string? Details { get; set; } = "Initial simulated product presence signal.";
}
