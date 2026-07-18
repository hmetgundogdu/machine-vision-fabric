namespace MachineVisionFabric.Contracts.Integrations;

public enum IntegrationCapabilityKind
{
    Source = 0,
    Gate = 1,
    Storage = 2,
    Telemetry = 3,
    Processor = 4,
    Sink = 5,

    /// <summary>Turns frame content into a control signal (perception → control).</summary>
    Classifier = 6
}
