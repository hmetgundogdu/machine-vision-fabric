namespace MachineVisionFabric.Cli.Tui;

public enum LogLevel { Info, Success, Warning, Error }

public sealed class PipelineLogEntry
{
    public required DateTime Timestamp { get; init; }
    public required LogLevel Level { get; init; }
    public required string Message { get; init; }
}
