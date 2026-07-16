# MachineVisionFabric Teknoloji Önerisi

## Kısa Sonuç

Önerilen ana teknoloji seçimi:

- `Pipeline motoru`: `C# / .NET 10 LTS`
- `UI / Studio`: `React + TypeScript + React Flow`
- `Desktop dağıtım`: ilk fazda zorunlu değil, gerekirse `Tauri` shell
- `AI inference`: `ONNX Runtime`
- `Streaming`: ilk fazda `MediaMTX`, özel ihtiyaç olursa `GStreamer`
- `Vendor SDK entegrasyonu`: `.NET adapter + native interop`
- `Python node`: ayrı process veya worker, aynı process değil

## Neden Bu Ayrım

En kritik teknik karar şu:

- kamera SDK ve gerçek zamanlı pipeline işi `client` tarafında olmamalı
- bu iş `headless runtime` içinde olmalı

Bunun nedeni:

- UI teknolojisi değişebilir
- ama kamera SDK, driver, frame yaşam döngüsü ve performans yönetimi daha sabit ve daha risklidir
- Linux ve Windows farklarını en temiz şekilde runtime adapter katmanında izole ederiz

Bu yüzden `UI` ile `engine` aynı teknoloji olmak zorunda değil.

## Pipeline Motoru İçin Öneri

### Seçim

- `C# / .NET 10 LTS`

### Gerekçe

- açık kaynak katkısı için erişilebilir dil ve ekosistem
- Windows ve Linux üzerinde güçlü servis geliştirme modeli
- kamera SDK'larını `P/Invoke` veya native wrapper ile bağlamak mümkün
- async, channels, memory pooling, hosted service yapıları gerçek zamanlıya yakın edge işlerde yeterince güçlü

### Neden .NET 8 Değil

Bugünün tarihi `16 Temmuz 2026`. Microsoft resmi sayfasına göre:

- `.NET 10` güncel `LTS`
- `.NET 8` desteği `10 Kasım 2026` tarihinde bitiyor

Yeni başlayan açık kaynak proje için `.NET 8` ile başlamak gereksiz erken migration borcu üretir.

Ek tasarım notu:

- pipeline motoru `headless Windows service` olarak çalışmaya uygun yazılmalı
- Linux desteği ikinci fazda aynı runtime abstractions üzerinden açılmalı

## Masaüstü Uygulaması İçin Öneri

### Ana öneri

Asıl ürün `masaüstü merkezli` değil, `web-first Studio` olmalı:

- `React + TypeScript`
- diagram tarafında `React Flow`

Sebep:

- node editor ve diagram ekosistemi web tarafında daha güçlü
- kullanıcılar Windows, Linux veya macOS üzerinde sadece tarayıcıyla erişebilir
- aynı UI merkezi panelde de, edge cihazda da çalışır

### Desktop gerekiyorsa

Zorunlu yerel paketleme ihtiyacı olursa:

- aynı web UI'ı `Tauri` ile sar

Ama önemli sınır:

- `Tauri` sadece shell olsun
- kamera SDK ve pipeline mantığı Tauri içine gömülmesin

## Neden Avalonia'yı Birincil Öneri Yapmıyorum

`Avalonia` kötü seçenek değil. Hatta saf masaüstü odaklı ürün olsa ciddi aday olurdu.

Ama bu proje için zayıf tarafı:

- gelişmiş diagram editörü web tarafında daha hızlı geliştirilecek
- merkezi panel ve edge panel için iki farklı UI stratejisi istemiyoruz
- topluluk katkısı için web tabanlı arayüz daha erişilebilir

Bu yüzden `Avalonia` ancak ikinci senaryoda mantıklı:

- tarayıcısız, tamamen yerel masaüstü operatör ekranı istiyorsak
- aynı UI kodunun C# içinde kalması bizim için çok önemliyse

## Kamera SDK ve Native Entegrasyon İçin Kural

Temel mimari kural:

- her vendor SDK ayrı adapter paketi olmalı
- adapter'lar capability manifest taşımalı
- her adapter hangi işletim sistemini desteklediğini açıkça söylemeli

Örnek:

- `rtsp` adapter: Windows + Linux
- `uvc` adapter: Windows + Linux
- `hikvision-mvs` adapter: vendor SDK'nın destek durumuna göre
- `cognex` adapter: vendor SDK'nın destek durumuna göre

Bu, açık kaynak projede çok önemli. Çünkü her kamera vendor'ı Linux desteği vermeyebilir. Çözüm dili değiştirmek değil, adapter sınırını doğru koymaktır.

Aynı kural PLC node'ları için de geçerli:

- `s7-control-node` gibi entegrasyonlar çekirdekten ayrılmalı
- ama graph içinde birinci sınıf node gibi davranmalı

## Python Node İçin Öneri

Python'ı doğrudan motor içine embed etmeyelim.

Daha güvenli model:

- `runtime` ana süreç
- `python worker` ayrı süreç
- veri aktarımı için shared memory veya düşük kopyalı buffer modeli

Böylece:

- Python çökse motor düşmez
- bağımlılıklar izole olur
- Linux ve Windows paketlemesi daha temiz olur

Bu worker modelinde iki yaşam döngüsü desteklenmeli:

- sürekli ayakta worker
- iş başına açılan worker

Çünkü cold start süreleri sahada doğrudan çevrim süresini etkiler.

## Streaming İçin Öneri

İlk faz:

- `MediaMTX`

İkinci faz veya özel media işleme gerekirse:

- `GStreamer`

Sebep:

- MediaMTX hazır protokol köprüsü sağlar
- GStreamer daha güçlüdür ama ilk günden daha karmaşıktır

## Net Teknoloji Kararı

Ben olsam bugün şu kararı veririm:

1. `Engine`: `.NET 10 LTS`
2. `UI`: `React + TypeScript + React Flow`
3. `Desktop package`: gerekirse `Tauri`
4. `Inference`: `ONNX Runtime`
5. `Media`: `MediaMTX` ile başla, gerektiğinde `GStreamer` ekle
6. `Python`: sidecar worker
7. `Vendor SDK`: adapter paketi + native interop

Bu seçim hem açık kaynak katkısını kolaylaştırır, hem Linux/Windows esnekliği sağlar, hem de kamera SDK riskini UI katmanından uzak tutar.

## Varsayılan Runtime Politikası

Node lifecycle için önerilen varsayılanlar:

- `camera/source`: resident
- `plc/control`: resident
- `ai-model`: resident ve preload
- `python/process helper`: on-demand
- `uzun warmup gerektiren external worker`: resident

Bu varsayılanlar kullanıcı tarafından değiştirilebilir; ama ilk davranış cold start maliyetini azaltacak yönde olmalı.

## Resmi Kaynaklar

- [.NET support policy](https://dotnet.microsoft.com/en-us/platform/support/policy)
- [.NET downloads](https://dotnet.microsoft.com/en-us/download/dotnet)
- [Avalonia supported platforms](https://docs.avaloniaui.net/docs/supported-platforms)
- [Avalonia cross-platform architecture](https://docs.avaloniaui.net/docs/fundamentals/cross-platform-architecture)
- [Tauri overview](https://v2.tauri.app/)
- [Tauri architecture](https://v2.tauri.app/concept/architecture/)
- [React Flow](https://reactflow.dev/)
- [ONNX Runtime docs](https://onnxruntime.ai/docs/)
- [ONNX Runtime execution providers](https://onnxruntime.ai/docs/execution-providers/)
- [P/Invoke in .NET](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke)
- [Native interop best practices](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/best-practices)
- [Python shared memory](https://docs.python.org/3/library/multiprocessing.shared_memory.html)
- [MediaMTX intro](https://mediamtx.org/docs/kickoff/introduction)
- [GStreamer RTSP server](https://gstreamer.freedesktop.org/documentation/gst-rtsp-server/index.html)
