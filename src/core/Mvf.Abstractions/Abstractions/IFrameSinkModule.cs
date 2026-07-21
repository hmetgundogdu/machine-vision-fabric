using System.Text.Json;
using Mvf.Graph.Integrations;

namespace Mvf.Abstractions;

/// <summary>
/// Integration module that acts as a terminal output/sink node in a pipeline.
/// Sinks receive frames from upstream data edges and produce no output ports.
/// </summary>
public interface IFrameSinkModule : IIntegrationModule
{
    /// <summary>
    /// Opens a new sink session for one pipeline execution run.
    /// The sink is responsible for managing its own output directory,
    /// session naming, and finalization.
    /// </summary>
    IFrameSink OpenSink(JsonElement configuration, string packageRoot);
}
