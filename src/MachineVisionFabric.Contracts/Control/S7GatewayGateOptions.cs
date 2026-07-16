namespace MachineVisionFabric.Contracts.Control;

public sealed class S7GatewayGateOptions
{
    public string Host { get; set; } = "127.0.0.1";

    public int Port { get; set; } = 15020;

    public int Rack { get; set; } = 0;

    public int Slot { get; set; } = 1;

    public S7SignalAddress Address { get; set; } = new();

    public string ProductPresentValue { get; set; } = "1";

    public string ProductAbsentValue { get; set; } = "0";

    public int ConnectTimeoutMs { get; set; } = 1500;

    public int ReadTimeoutMs { get; set; } = 1500;

    public string SourceName { get; set; } = "s7-gateway-gate";

    public string StationId { get; set; } = "station-1";
}
