# MachineVisionFabric Mimari Temeli

## 1. Problem Tanımı

Hedef sistem şunları aynı çatı altında çözmeli:

- Farklı kaynaklardan görüntü veya frame akışı almak
- Bu akışlar için konfigürasyon, tetikleme ve iş kuralları tanımlamak
- Diyagram tabanlı pipeline kurmak
- Python, exe, AI model ve yerel işlem node'larını aynı graph içinde çalıştırmak
- Hem ham stream'i hem de işlenmiş çıktıları başka sistemlere ağ üzerinden sunmak

Bu nedenle ürün tek bir "kamera uygulaması" değil, bir `vision integration and execution platform` olarak tasarlanmalı.

## 1.1 Dağıtım Varsayımı

Bu proje için yeni netleşen operasyonel varsayım:

- sistem şirket içi ağlarda çalışacak
- hedef cihazlar panel PC, endüstriyel PC ve Intel NUC sınıfı edge makineler
- ilk faz `Windows-first`
- bazı kurulumlar internete çıkmadan veya çok sınırlı erişimle çalışabilecek
- düşük bakım gerektiren yerel servis modeli tercih edilmeli

Bu bilgi mimaride doğrudan etkili. Tasarım `cloud-first` değil, `edge-first and LAN-native` olmalı.

## 2. Önerilen Üst Mimari

### 2.1 Katmanlar

1. `Control Plane`
   Kullanıcıların pipeline tanımladığı, config yönettiği, node eklediği ve tetikleme kurallarını düzenlediği katman.
2. `Execution Plane`
   Aktif graph'ları çalıştıran runtime.
3. `Media Plane`
   Görüntü alma, dönüştürme, encode etme ve yayınlama katmanı.
4. `Extension Plane`
   Python, exe, yerel plugin ve vendor adapter eklenti modeli.

## 2.2 Deployment Yaklaşımı

Panel PC ve NUC hedefi nedeniyle başlangıç dağıtım modeli şu olmalı:

- tek cihazda çalışan `Runtime + Api`
- aynı cihazda veya tarayıcıdan erişilen `Studio`
- ihtiyaç varsa aynı cihazda veya ayrı edge node'da `Media Gateway`

Bu, ilk tasarımda tam mikroservis mimarisine gitmememiz gerektiği anlamına gelir. Öncelik:

- tek makinede güvenilir çalışma
- düşük RAM ve CPU baskısı
- servis olarak otomatik ayağa kalkma
- ağ kesintisinde kendi başına çalışmaya devam etme

## 2.3 Merkezi Sistem İçin Hibrit Yaklaşım

Merkezi bir yapı tamamen yanlış değil; ancak merkezi node'un pipeline çalıştıran zorunlu beyin olması saha için riskli.

Daha doğru model:

- `edge node` kamera, inference, trigger ve yerel pipeline yürütür
- `central control` cihaz envanteri, log, audit, health, versiyon bilgisi ve isteğe bağlı yedekleme toplar
- pipeline tanımı merkezden zorunlu push edilmez
- pipeline dosyaları edge cihazlarda `import/export` ile taşınabilir
- istenirse merkez sadece önerilen veya onaylı pipeline paketlerini yayınlar

Bu modelde client tarafı sabit kalır; değişen şey yüklenen pipeline paketi ve adapter konfigürasyonudur.

## 2.4 Neden Tam Merkezi Orchestrator Önermiyorum

Şirket içi ağ ve panel PC/NUC dağıtımı için tam merkezi tasarımın riskleri:

- ağ kesintisinde üretim hattı etkilenir
- her kamera tetik ve inference kararı merkezden geçerse gecikme artar
- saha devreye alma daha kırılgan olur
- tek nokta arızası oluşur
- vendor SDK ve donanım sürücüleri çoğu zaman zaten edge makinede olmak zorundadır

Bu yüzden merkez, `runtime authority` değil `management authority` olmalı.

## 2.5 Önerilen Hibrit Sınır

Edge cihazda kalması gerekenler:

- kamera bağlantısı
- frame yakalama
- trigger engine
- graph execution
- AI inference
- yerel stream üretimi
- kısa süreli buffer ve yerel persist
- opsiyonel telemetry sinyal yayını

Merkezde olması mantıklı olanlar:

- cihaz kaydı ve envanter
- sürüm ve paket kataloğu
- log toplama
- alarm ve olay görünürlüğü
- health durumu
- yedek konfigürasyon depolama
- opsiyonel pipeline şablon kütüphanesi
- opsiyonel canlı pipeline sinyal izleme

Merkezde olmaması gerekenler, en azından MVP'de:

- canlı frame path üzerinde zorunlu karar mekanizması
- her node çalıştırmasını uzaktan yönetme
- her tetik için merkezi onay

## 2.6 Opsiyonel Pipeline Signal Streaming

Merkez tarafı isterse edge node'un pipeline sinyallerini izleyebilmeli. Ancak bu izleme modeli şu kurallara bağlı olmalı:

- yayın tamamen opsiyonel olmalı
- varsayılan durumda kapalı gelmeli
- edge runtime'ın hot path akışını bloklamamalı
- merkez bağlı değilse veya yavaşsa edge çalışması etkilenmemeli

İzlenebilecek sinyal türleri:

- `pipeline.started`
- `pipeline.stopped`
- `node.started`
- `node.completed`
- `node.failed`
- `trigger.fired`
- `frame.received`
- `inference.completed`
- `stream.published`
- `storage.saved`

Bu sinyallerin amacı gözlemlemedir; çalıştırma otoritesi değildir.

## 2.7 Performans Kuralı

Signal streaming için temel kural:

- inference veya frame işleme thread'i doğrudan ağa yazmamalı

Doğru yaklaşım:

- runtime olay üretir
- olaylar lock-free veya bounded queue benzeri hafif bir telemetry buffer'a yazılır
- ayrı bir background publisher bu olayları merkez isteyen abonelere iletir
- buffer dolarsa sinyal düşebilir; pipeline çalışması düşmemeli

Bu modelde öncelik sırası:

1. pipeline execution
2. local safety and persistence
3. optional observability export

Yani telemetry her zaman `best effort` olmalı, `mission critical` değil.

## 3. Ana Bileşenler

### 3.1 Source Adapters

Her görüntü kaynağı ortak bir sözleşmeye uymalı:

- `camera`
- `rtsp`
- `usb/uvc`
- `mjpeg/http`
- `file/replay`
- `shared-memory`
- `custom vendor sdk`

Her adapter şu yetenekleri ilan etmeli:

- discovery destekliyor mu
- config anahtarları neler
- trigger destekliyor mu
- pull mü push mu çalışıyor
- frame formatları neler
- reconnect stratejisi nasıl

Şu an için vendor entegrasyonları ürünün çekirdeğine gömülmüş sabit bileşenler olarak değil, sonradan eklenebilen örnek adapter paketleri olarak düşünülmeli. Yani ilk yapı:

- çekirdek runtime vendor bağımsız
- kamera ve PLC gibi saha entegrasyonları adapter/node örneği olarak eklenebilir
- bu örnekler topluluk için referans teşkil eder

### 3.2 Trigger Engine

Trigger sistemi ayrı bir çekirdek olmalı. Sadece PLC veya kamera trigger'ı olarak düşünülmemeli.

Önerilen model:

- `Event`: `frame.received`, `timer.elapsed`, `plc.signal.changed`, `http.requested`, `node.completed`
- `Condition`: config tabanlı filtre veya expression
- `Action`: capture, node çalıştır, branch değiştir, stream başlat, alarm üret

Bu sayede kullanıcı runtime sırasında yeni trigger kuralı ekleyebilir.

### 3.3 Graph Runtime

Pipeline lineer değil yönlü bir graph olmalı.

Node tipleri:

- `source`
- `transform`
- `ai-model`
- `python-step`
- `process-step`
- `router`
- `aggregator`
- `stream-output`
- `storage-output`
- `event-output`

Graph runtime ilk aşamada şu kurallarla başlamalı:

- her edge veri tipi taşımalı
- node input/output contract'ı şemalı olmalı
- execution bounded queue mantığı ile yürümeli
- her node için timeout, retry ve concurrency limiti olmalı

### 3.4 Pipeline Soyutlaması İçin Önerilen Temel Model

Pipeline yeterince esnek olmalı; ama "her şey her şeye bağlanabilir" seviyesinde gevşek olmamalı. Önerilen soyutlama:

- `control-flow`
- `data-flow`
- `strict typed ports`

Yani graph içinde iki farklı edge tipi olmalı:

- `data edge`: frame, tensor, metadata, result gibi veri taşır
- `control edge`: karar, geçiş, kapı açma, tetikleme sonucu gibi akış kontrolü taşır

Ve her bağlantı tek yönlü, tip kontrollü olmalı:

- `NODE(typed output) -> NODE(typed input)`
- sadece uyumlu tipler birbirine bağlanabilmeli
- bağlantı anında şema doğrulaması yapılmalı
- runtime sırasında da tip doğrulaması korunmalı

Bu ayrım özellikle PLC node'ları için kritik.

Örnek:

- `PLC Presence Node` istasyonda ürün var mı yok mu bilgisini üretir
- bu node doğrudan görüntü işlemez
- ama `next branch` kararını verir

Bu yüzden pipeline'da sadece görüntü node'ları değil şu sınıflar olmalı:

- `source node`
- `control node`
- `compute node`
- `integration node`
- `output node`

### 3.4.1 Şema Disiplini

Node sözleşmesi gevşek `key-value` geçişlerine dayanmamalı. Bunun yerine:

- her input port'un tipi tanımlı olmalı
- her output port'un tipi tanımlı olmalı
- config ayrı bir şema olmalı
- runtime parametresi ile data payload birbirine karışmamalı

Örnek yaklaşım:

- `Frame`
- `DetectionList`
- `BooleanGate`
- `StationPresence`
- `InferenceResult`
- `StreamPacket`

Bu yaklaşımın faydası:

- diagram tarafında yanlış bağlantılar erken engellenir
- node yazarları net contract ile çalışır
- import/export paketleri daha güvenilir olur
- gelecekte çoklu dil node desteği daha temiz kurulur

### 3.5 PLC Node İçin Yapısal Karar

S7-200 gibi PLC'ler ilk fazda "çekirdek runtime parçası" değil, örnek bir `control node adapter` olarak ele alınmalı.

Bu node tipinin sorumluluğu:

- PLC'ye bağlanmak
- belirli register/bit okumak
- ürün var/yok gibi istasyon durumunu çıkarmak
- sonucu pipeline kontrol sinyali olarak yayınlamak

Bu yapı sayesinde PLC sadece I/O sürücüsü olmaz; graph içindeki akış karar mekanizmasına dönüşür.

Örnek kullanım:

- `PLC Presence Node -> if product present -> Trigger Capture Node -> AI Model Node`
- `PLC Presence Node -> if no product -> idle branch`

Bu yaklaşım pipeline esnekliğini artırır ve gelecekte aynı mantığın farklı sensör node'ları ile tekrar kullanılmasını sağlar.

### 3.6 Node Contract Önerisi

Her node en az şu niteliklere sahip olmalı:

- `nodeType`
- `capabilities`
- `input schema`
- `output schema`
- `config schema`
- `lifecycle policy`
- `health contract`

Özellikle `lifecycle policy` artık zorunlu düşünülmeli.

## 3.7 Lifecycle Policy ve Cold Start Tasarımı

Yeni önemli gereksinim:

- process, model ve entegrasyon bileşenleri cold start maliyetini azaltacak biçimde çalışmalı

Bu yüzden node veya adapter çalıştırma modeli ikili olmalı:

- `resident`
- `on-demand`

`resident`:

- process veya bağlantı sürekli ayakta tutulur
- PLC bağlantısı, model belleğe alma, uzun warmup gerektiren inference engine için uygundur

`on-demand`:

- ihtiyaç anında ayağa kalkar
- kısa, seyrek, maliyeti düşük yardımcı işler için uygundur

Node standardında şu alanlar olmalı:

- `activationMode`: `resident` | `on-demand`
- `warmupPolicy`
- `idleTimeout`
- `preloadOnPipelineStart`
- `healthProbe`

Bu sayede:

- PLC node sürekli bağlı kalabilir
- AI model node pipeline açılırken önceden belleğe alınabilir
- kısa yaşayan yardımcı process node'ları gerektiğinde açılabilir

Buradaki ana karar motor içinde sabit kod olmamalı; node descriptor ile tanımlanmalı.

Varsayılan politika önerisi:

- `AI model node`: `resident` + `preloadOnPipelineStart = true`
- `PLC control node`: `resident`
- `camera/source node`: `resident`
- `short helper process node`: `on-demand`
- `heavy external process node`: varsayılan `resident`, ama override edilebilir

Gerekçe:

- kamera, PLC ve model node'larında cold start çevrim süresini doğrudan bozar
- kısa yardımcı işler için sürekli ayakta proses tutmak gereksiz kaynak tüketir

## 4. Neden .NET Tabanlı Çekirdek

İlk öneri, orchestration çekirdeğini `.NET 10` üzerinde kurmak.

Gerekçeler:

- mevcut referans proje zaten .NET tabanlı
- Windows vendor SDK entegrasyonları bu tarafta daha doğal
- yüksek eşzamanlılık ve servis mimarisi için güçlü
- web API, worker service ve native process kontrolü tek ekosistemde çözülebilir

Microsoft destek politikasına göre çift numaralı .NET sürümleri LTS, tek numaralı sürümler STS; resmi sayfa şu anda `.NET 10`'u güncel sürüm olarak gösteriyor. Kaynak:

- [Microsoft .NET support policy](https://dotnet.microsoft.com/en-us/platform/support/policy/dotnet-core)
- [Microsoft .NET 7 download page](https://dotnet.microsoft.com/en-us/download/dotnet/7.0)

Not: Buradaki öneri `.NET 10` üzerine gitmek; fakat donanım veya kurumsal kısıt varsa `.NET 8` LTS tabanlı kalmak da makul.

Panel PC ve NUC dağıtımı açısından pratik varsayım:

- ilk faz için `Windows-first`
- ikinci faz için Linux edge desteği opsiyonel

Bunun nedeni vendor camera SDK ve saha cihazı sürücü bağımlılıklarının çoğunlukla Windows tarafında daha rahat çözülmesi.

## 5. Diagram Sistemi

Web tabanlı bir diagram editörü öneriyorum.

İlk aday:

- `React Flow`

Sebep:

- node-based editor odaklı
- MIT lisanslı açık kaynak
- custom node, edge, minimap ve interaction modeli hazır

Kaynak:

- [React Flow](https://reactflow.dev/)

UI kararında amaç görsel güzellik değil, graph tanımı için sağlam bir editör altyapısı seçmek.

## 6. Streaming Stratejisi

Eski projedeki özel UDP/TCP snapshot protokolü yerine standart media protokollerine geçmek daha doğru.

Önerilen ayrım:

- makine içi işleme: ham frame / shared buffer / raw tensor
- ağa yayın: standart media server

İlk yaklaşım:

- runtime görüntüyü `Media Gateway` katmanına verir
- gateway RTSP, WebRTC, SRT gibi protokollere çevirir
- pipeline çıktısı da ayrı stream veya metadata kanalı olarak yayınlanır

Güncel referanslar:

- GStreamer `appsrc` uygulamanın dışarıdan pipeline'a veri itmesine izin verir: [appsrc docs](https://gstreamer.freedesktop.org/documentation/app/appsrc.html)
- GStreamer `rtspsrc` RTSP akışı alma tarafını standartlaştırır: [rtspsrc docs](https://gstreamer.freedesktop.org/documentation/rtsp/rtspsrc.html)
- `gst-rtsp-server` RTSP servis yayınlayabilir: [gst-rtsp-server docs](https://gstreamer.freedesktop.org/documentation/gst-rtsp-server/rtsp-server.html)
- Web istemcileri için WebRTC tarayıcı desteği güçlüdür: [WebRTC overview](https://webrtc.org/) ve [MDN WebRTC API](https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API)
- MediaMTX tek sunucuda RTSP, WebRTC, SRT, RTMP, HLS ve diğer protokoller arasında köprü kurabiliyor: [MediaMTX publish](https://mediamtx.org/docs/features/publish), [MediaMTX read](https://mediamtx.org/docs/features/read)

Pratik öneri:

- MVP için doğrudan GStreamer yazmak yerine önce `MediaMTX` sidecar ile yayın mimarisini kur
- gerçekten özel encode/transform ihtiyacı doğarsa GStreamer tabanlı özel media node ekle

Bu, ilk sürüm karmaşıklığını ciddi biçimde azaltır.

## 7. AI Model Runtime

AI modeller node olarak ele alınmalı; uygulama içine gömülü tek inference akışı gibi değil.

Desteklenecek ilk model tipleri:

- `onnx`
- `python-runtime`
- `external process`

Performans için ONNX Runtime tarafında bellek bağlama ve ön tahsis stratejileri önemli. Resmi dökümantasyonda `IOBinding`, giriş ve çıkışların önceden ayrılmış belleğe bağlanmasını öneriyor:

- [ONNX Runtime I/O Binding](https://onnxruntime.ai/docs/performance/tune-performance/iobinding.html)
- [ONNX Runtime C# API](https://onnxruntime.ai/docs/api/csharp/api/Microsoft.ML.OnnxRuntime.OrtIoBinding.html)

Bu yüzden AI node standardında şu alanlar olmalı:

- model formatı
- input tensor sözleşmesi
- output schema
- device tercihi
- batch ve warmup ayarları
- preload ve unload politikası

## 8. Dağıtık Event Omurgası

Tek makine ile başlanabilir; ancak mimari dağıtık çalışmaya açık olmalı.

Öneri:

- tek makine başlangıcı: in-process event bus
- çok süreçli veya ağ dağıtımı: opsiyonel `NATS`

NATS resmi dokümanında yüksek performanslı, hafif ve açık kaynak bir messaging katmanı olarak tanımlanıyor; ayrıca pub/sub, request/reply ve persistence için JetStream sunuyor:

- [NATS docs](https://docs.nats.io/)
- [NATS overview](https://docs.nats.io/nats-concepts/overview)
- [NATS JetStream](https://docs.nats.io/nats-concepts/jetstream)

Bu, pipeline event'leri, trigger sinyalleri ve metadata dağıtımı için güçlü bir aday.

Ancak panel PC ve NUC yerleşiminde bunu ilk günden zorunlu yapmamalıyız.

İlk tercih:

- tek cihaz: in-process event bus
- aynı ağda çoklu cihaz: opsiyonel NATS
- merkezi zorunluluk yok

Hibrit modelde NATS benzeri bir katman varsa bunun rolü:

- telemetry ve event dağıtımı
- merkezi gözlemleme
- opsiyonel komut iletimi

Ama pipeline çalıştırma mantığı yine edge node içinde kalmalı.

Opsiyonel pipeline sinyal yayını için alternatifler:

- ilk faz: HTTP SSE veya WebSocket ile yerel publish
- ikinci faz: NATS veya benzeri event bus ile çoklu izleyici desteği

Burada önemli olan sinyal kanalını video kanalından ayırmaktır. Video başka, telemetry başka taşınmalı.

## 9. Önerilen Repo Yapısı

```text
MachineVisionFabric/
├─ docs/
├─ src/
│  ├─ MachineVisionFabric.Contracts/
│  ├─ MachineVisionFabric.Core/
│  ├─ MachineVisionFabric.Runtime/
│  ├─ MachineVisionFabric.Adapters/
│  ├─ MachineVisionFabric.Streaming/
│  ├─ MachineVisionFabric.Api/
│  └─ MachineVisionFabric.Host/
├─ ui/
│  └─ machine-vision-fabric-studio/
├─ sdk/
│  ├─ python/
│  └─ process/
├─ samples/
│  ├─ pipelines/
│  └─ adapters/
└─ tools/
```

## 10. MVP Sınırı

İlk sürümde her şeyi çözmeye çalışma.

MVP:

1. bir source adapter
2. bir ai-model node
3. bir python-step node
4. bir stream-output node
5. bir storage-output node
6. basit web diagram editörü
7. pipeline JSON export/import
8. temel trigger engine

İlk desteklenecek akış için öneri:

`RTSP veya simüle kamera -> preprocess -> ONNX model -> overlay -> stream + save + event`

Bu akış oturduğunda vendor kamera ve PLC entegrasyonları eklenir.

MVP deployment hedefi:

- tek panel PC veya tek NUC üzerinde servis olarak çalışan runtime
- aynı cihazda yerel web paneli
- başka istemcilerin LAN üzerinden stream veya metadata alabilmesi
- merkezden canlı gözlemlenebilmesi, ancak çalışmak için merkeze ihtiyaç duymaması

Hibrit modele göre MVP sonrası ilk büyüme adımı:

- birden fazla edge node'u gören merkezi izleme paneli
- edge cihazların export ettiği pipeline paketlerini arşivleyen bir merkez
- log ve health görünürlüğü
- edge node'lardan gelen opsiyonel pipeline signal stream görünürlüğü

Bu aşamada bile pipeline import/export edge merkezli kalmalı.

## 11. Pipeline Paketleme

Import/export sadece tek JSON dosyası olmamalı. Çünkü gerçek sahada pipeline ile birlikte ek varlıklar taşınacak:

- AI model dosyaları
- label ve config dosyaları
- Python script'leri
- yardımcı executable'lar
- adapter manifestleri

Bu yüzden önerilen model:

- `manifest JSON`
- bunu çevreleyen `folder package`

Örnek:

```text
pipeline-package/
├─ pipeline.json
├─ assets/
│  ├─ models/
│  ├─ scripts/
│  ├─ processes/
│  └─ configs/
└─ manifest.json
```

İlk aşamada klasör bazlı taşıma yeterli. Daha sonra istersek bunu tek arşiv formatına da çevirebiliriz.

## 12. İlk Simulator Stratejisi

Gerçek kamera ilk MVP için zorunlu değil. Bunun yerine birden fazla simülasyon kaynağı ile çekirdek doğrulanmalı.

Önerilen ilk simulator tipleri:

- `single-image-loop`
- `folder-sequence-camera`
- `side-by-side-multi-frame simulator`
- `scenario-based simulator`

Özellikle `folder-sequence-camera` şu davranışı vermeli:

- seçilen klasördeki görüntüleri sırayla oynatır
- istenirse döngüsel tekrar yapar
- frame interval ayarlanabilir
- birden fazla sanal kamera aynı anda çalışabilir

`scenario-based simulator` ise ileride şu işleri destekleyebilir:

- ürün var / ürün yok senaryosu
- tetik gecikmesi
- PLC bit değişimi ile senkron akış
- hata, timeout veya boş frame senaryosu

## 13. Netlesen Ek Kararlar

Bu analiz turunda sabitlenen ek kararlar:

- `control edge` ayrı görselleştirilecek
- telemetry için ilk tercih `WebSocket`
- raw TCP tabanlı özel telemetry protokolü ilk faz için gereksiz
- external process node'ları cold start maliyetine göre `resident` veya `on-demand` seçebilecek

Telemetry tarafında tercih sırası:

1. `WebSocket`
2. gerekirse daha sonra event bus

Sebep:

- yerel web paneli ile doğal uyum
- şirket içi ağda yeterli performans
- uygulama maliyetinin düşük olması

Node lifecycle tarafında net varsayılan:

- `camera/source`: resident
- `plc/control`: resident
- `ai-model`: resident + preload
- `short helper process`: on-demand
- `heavy external process`: varsayılan resident, override edilebilir
