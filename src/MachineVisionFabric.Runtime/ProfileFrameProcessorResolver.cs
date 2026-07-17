using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Core.Abstractions;
using Microsoft.Extensions.Logging;

namespace MachineVisionFabric.Runtime;

public sealed class ProfileFrameProcessorResolver(
    IIntegrationModuleLoader integrationModuleLoader,
    ILogger<ProfileFrameProcessorResolver> logger) : IFrameProcessorResolver
{
    public FrameProcessorResolution Resolve(FabricProfileManifest manifest, string integrationsRoot)
    {
        var binding = manifest.FrameProcessor;
        if (!string.Equals(binding.Mode, "module", StringComparison.OrdinalIgnoreCase))
        {
            logger.LogInformation("Resolved frame processor as disabled.");
            return new FrameProcessorResolution(null, "none", "none");
        }

        var modules = integrationModuleLoader.LoadModules(integrationsRoot)
            .OfType<IFrameProcessorModule>()
            .ToArray();

        var selectedModule = modules.FirstOrDefault(module =>
            string.Equals(module.Describe().ModuleId, binding.ModuleId, StringComparison.OrdinalIgnoreCase));

        if (selectedModule is null)
        {
            throw new InvalidOperationException(
                $"Frame processor module '{binding.ModuleId}' could not be found under '{integrationsRoot}'.");
        }

        logger.LogInformation(
            "Resolved frame processor through external module {ModuleId}.",
            selectedModule.Describe().ModuleId);

        return new FrameProcessorResolution(
            selectedModule.CreateProcessor(binding.Config),
            selectedModule.Describe().ModuleId,
            "module");
    }
}
