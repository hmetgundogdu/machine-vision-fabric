# AutomationAiRunnerProgram2 Üzerinden Çıkan Bulgular

İncelenen kaynak proje:

- `C:\Users\c9018243a\Downloads\AutomationAiRunnerProgram2`

## Güçlü Taraflar

Eski projede korunması gereken bazı doğru kararlar var:

- `Core / Application / Infrastructure / UI` ayrımı yapılmış
- Kamera, PLC ve AI model erişimi interface tabanlı tasarlanmış
- `System.Threading.Channels` ile backpressure mantığı kullanılmış
- Inference, PLC yazımı ve persistence ayrı worker akışlarına ayrılmış
- Simüle kamera ve simüle PLC desteği düşünülmüş
- Ağ üzerinden inspection sonucu yayınlama mantığı erken aşamada ele alınmış

Özellikle [InspectionPipeline.cs](C:\Users\c9018243a\Downloads\AutomationAiRunnerProgram2\src\AutomationAiRunner.Application\Pipeline\InspectionPipeline.cs) dosyasındaki ayrım önemli:

- `InferenceWorkerAsync`
- `PlcWorkerAsync`
- `PersistenceWorkerAsync`

Bu ayrım yeni projede daha genel bir `graph execution runtime` yapısına evrilebilir.

## Sınırlayıcı Taraflar

Yeni hedefler açısından eski projede bazı yapısal sınırlar var:

- Pipeline lineer; graph tabanlı değil
- Trigger sistemi statik; runtime sırasında yeni kural eklemek için uygun değil
- Kamera keşfi ve entegrasyonları derleme zamanında sabit
- Dağıtık yayın mantığı proje özel protokole dayanıyor, standart streaming protokollerine dayanmıyor
- UI masaüstü merkezli; açık kaynak topluluk katkısı için web tabanlı editör daha uygun
- AI modeli bir node tipi gibi değil, uygulamanın ana omurgasına daha sıkı bağlı

## DynoVisionPipeline İçin Çıkarım

Yeni projede korunacak fikirler:

- Adaptör bazlı donanım sınırları
- Backpressure ve bounded queue mantığı
- Simülasyon modu
- Pipeline health/watchdog yaklaşımı
- Persist ve stream görevlerini inference yolundan ayırmak

Yeni projede değişecek temel noktalar:

- Lineer pipeline yerine directed graph runtime
- Sadece kamera değil her türlü stream ve frame source için birleşik source contract
- Trigger mantığını event + condition + action modeliyle dinamik yapmak
- AI, Python ve exe adımlarını ortak node sözleşmesine taşımak
- Ağ yayınını RTSP, WebRTC, SRT gibi standartlar üzerinden sunmak
