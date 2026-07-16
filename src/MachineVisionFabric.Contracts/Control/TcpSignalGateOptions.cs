namespace MachineVisionFabric.Contracts.Control;

public sealed class TcpSignalGateOptions
{
    public string Host { get; set; } = "127.0.0.1";

    public int Port { get; set; } = 15020;

    public string RequestPayload { get; set; } = "READ product.present";

    public string ProductPresentValue { get; set; } = "1";

    public string ProductAbsentValue { get; set; } = "0";

    public int ConnectTimeoutMs { get; set; } = 1500;

    public int ReadTimeoutMs { get; set; } = 1500;

    public string SourceName { get; set; } = "tcp-plc-gate";

    public string StationId { get; set; } = "station-1";
}
