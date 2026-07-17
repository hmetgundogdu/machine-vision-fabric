using MachineVisionFabric.Host;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Runtime;
using MachineVisionFabric.Runtime.Pipelines;
using MachineVisionFabric.Runtime.Plugins;
using MachineVisionFabric.Storage;

var builder = Host.CreateApplicationBuilder(args);
builder.Configuration.Sources.Clear();
builder.Configuration
    .SetBasePath(AppContext.BaseDirectory)
    .AddJsonFile("appsettings.json", optional: false, reloadOnChange: false)
    .AddJsonFile($"appsettings.{builder.Environment.EnvironmentName}.json", optional: true, reloadOnChange: false)
    .AddEnvironmentVariables()
    .AddCommandLine(args);
builder.Services.Configure<MachineVisionFabricRuntimeOptions>(
    builder.Configuration.GetSection(MachineVisionFabricRuntimeOptions.SectionName));
builder.Services.AddSingleton<IPackageManifestLoader, PackageManifestLoader>();
builder.Services.AddSingleton<IEntryProfileLoader, EntryProfileLoader>();
builder.Services.AddSingleton<IDatasetSessionPreparer, DatasetSessionPreparer>();
builder.Services.AddSingleton<IDatasetCollector, DatasetCollector>();
builder.Services.AddSingleton<ISimulatorSourceCatalog, EmptySimulatorSourceCatalog>();
builder.Services.AddSingleton<IIntegrationModuleLoader, IntegrationModuleLoader>();
builder.Services.AddSingleton<IFrameSourceResolver, ProfileFrameSourceResolver>();
builder.Services.AddSingleton<IProductPresenceGateResolver, ProfileProductPresenceGateResolver>();
builder.Services.AddSingleton<IFrameProcessorResolver, ProfileFrameProcessorResolver>();
builder.Services.AddSingleton<DatasetCaptureCompatibilityPipelineFactory>();
builder.Services.AddSingleton<IPipelineDefinitionProvider, PackagePipelineDefinitionProvider>();
builder.Services.AddSingleton<IPipelineDefinitionValidator, PipelineDefinitionValidator>();
builder.Services.AddSingleton<IPipelineInspectionService, PipelineInspectionService>();
builder.Services.AddSingleton<IHeadlessRuntimeBootstrapper, HeadlessRuntimeBootstrapper>();
builder.Services.AddHostedService<Worker>();

var host = builder.Build();
host.Run();
