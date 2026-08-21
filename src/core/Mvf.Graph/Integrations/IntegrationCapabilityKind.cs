namespace Mvf.Graph.Integrations;

public enum IntegrationCapabilityKind
{
    Source = 0,
    Gate = 1,
    Storage = 2,
    Telemetry = 3,
    Processor = 4,
    Sink = 5,

    /// <summary>Turns frame content into a control signal (perception → control).</summary>
    Classifier = 6,

    /// <summary>
    /// Consumes one frame and may emit a derived frame, a routing decision, and structured inference
    /// metadata in the same cycle.
    /// </summary>
    Analyzer = 7
}
