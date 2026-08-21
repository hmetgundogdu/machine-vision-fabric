using System.Text.Json;
using Mvf.Graph.Integrations;
using Mvf.Abstractions;

namespace Mvf.Sdk;

/// <summary>
/// Base class for integration modules that analyze one frame and may emit a derived frame, a control
/// classification, and structured inference metadata in one cycle.
/// </summary>
public abstract class FrameAnalyzerModuleBase<TOptions> : IFrameAnalyzerModule
    where TOptions : class, new()
{
    public IntegrationModuleDescriptor Describe()
    {
        return BuildDescriptor();
    }

    public IFrameAnalyzer CreateAnalyzer(JsonElement configuration)
    {
        return CreateAnalyzer(JsonConfigurationParser.Parse<TOptions>(configuration));
    }

    protected abstract IntegrationModuleDescriptor BuildDescriptor();

    protected abstract IFrameAnalyzer CreateAnalyzer(TOptions options);
}
