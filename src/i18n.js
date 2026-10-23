// Arayüz dili. Anahtar = İngilizce metin; `en` sözlüğü bu yüzden boştur, her
// anahtar kendisini İngilizce olarak döndürür. Türkçe çevirisi olmayan bir
// anahtar İngilizce görünür — uydurma çeviri yazılmaz.
export const LANGUAGES = [
  { id: "tr", label: "Türkçe", short: "TR" },
  { id: "en", label: "English", short: "EN" },
];

const tr = {
  // Gezinme
  Connections: "Bağlantılar",
  Routing: "Yönlendirme",
  "VS Code Extensions": "VS Code Eklentileri",
  Profiles: "Profiller",
  Logs: "Günlükler",
  Settings: "Ayarlar",
  "Main menu": "Ana menü",
  "Providers": "Sağlayıcılar",
  "Search or run a command": "Ara veya komut çalıştır",
  "Agent running": "Agent çalışıyor",
  "Agent stopped": "Agent kapalı",

  // Sayfa başlıkları
  "Connect your providers to Codex, OpenCode and Claude Code. Config files are backed up.":
    "Sağlayıcılarınızı Codex, OpenCode ve Claude Code’a bağlayın. Konfigürasyon dosyaları yedeklenir.",
  "Add providers. Keys stay in a file only you can read; they are never written to config files.":
    "Sağlayıcıları ekleyin. Anahtarlar yalnızca sizin okuyabileceğiniz bir dosyada kalır, config dosyalarına yazılmaz.",
  "Inject the route you selected into the editor extension, independently of CLI profiles.":
    "CLI profillerinden bağımsız olarak seçtiğiniz rotayı editör eklentisine enjekte edin.",
  "Routing sets for different working environments.":
    "Farklı çalışma ortamları için yönlendirme setleri.",
  "Review connection tests and configuration changes.":
    "Bağlantı testlerini ve yapılandırma değişikliklerini inceleyin.",
  "Manage backups, security and application preferences.":
    "Yedekleme, güvenlik ve uygulama tercihlerini yönetin.",

  // Yönlendirme
  "Routing matrix": "Yönlendirme matrisi",
  "Choose which provider and model goes to which tool.":
    "Hangi sağlayıcı ve modelin hangi araca gideceğini belirleyin.",
  "Reset to defaults": "Varsayılana dön",
  Harness: "Harness",
  Model: "Model",
  Status: "Durum",
  Ready: "Hazır",
  "Key missing": "Anahtar eksik",
  "Complete the connections": "Bağlantıları tamamla",
  "Last test": "Son test",
  "Not tested yet": "Henüz test edilmedi",
  "Selected route": "Seçili yönlendirme",
  Provider: "Sağlayıcı",
  Default: "Varsayılan",
  Models: "Modeller",
  Connected: "Bağlı",
  "Not connected": "Bağlantı yok",
  "API key required": "API anahtarı gerekli",
  "Test connection": "Bağlantıyı test et",
  Testing: "Test ediliyor...",
  "Connection failed": "Bağlantı başarısız",
  "Connection successful": "Bağlantı başarılı",
  "This route has not been tested yet. The test measures latency with a real request.":
    "Bu rota henüz test edilmedi. Test, gerçek bir istekle gecikmeyi ölçer.",
  "The test measures latency with a real request.":
    "Test, gerçek bir istekle gecikmeyi ölçer.",
  "Files to change": "Değiştirilecek dosyalar",
  "CLI profile": "CLI profili",
  "The config file is not changed. The route is injected only into the process started with Turnout.":
    "Config dosyası değişmez. Rota yalnızca Turnout ile başlatılan sürece enjekte edilir.",
  "Save CLI profile": "CLI profilini kaydet",
  Saving: "Kaydediliyor...",
  "Keys are kept in": "Anahtarlar",
  "with user-only access.": "içinde, yalnızca kullanıcı erişimiyle tutulur.",

  // Model seçici
  "Active models": "Etkin modeller",
  "Recommended set of models": "Önerilen model seti",
  "Changing the provider keeps earlier selections. Every selected model appears in the harness menu.":
    "Sağlayıcı değiştirince önceki seçimler korunur. Seçilen tüm modeller harness menüsünde görünür.",
  "original list kept": "orijinal liste korunuyor",
  "connection required": "bağlantı yok",

  // Uzantılar
  "A separate, explicit action.": "Ayrı ve açık bir işlem.",
  "The buttons on this screen change the harness's shared config file with a backup and prepare the Turnout Agent. The CLI profile on the Routing screen does not touch these files.":
    "Bu ekrandaki düğmeler harness’ın paylaşılan config dosyasını yedekleyerek değiştirir ve Turnout Agent’ı hazırlar. Yönlendirme ekranındaki CLI profili bu dosyalara dokunmaz.",
  "Connections incomplete": "Bağlantı eksik",
  Config: "Config",
  "Transfer to VS Code extension": "VS Code eklentisine aktar",
  "Connections must be completed": "Bağlantı tamamlanmalı",
  Injecting: "Enjekte ediliyor...",
  "Keep Codex's own models": "Codex’in kendi modellerini koru",
  "Keep Claude's own models": "Claude’un kendi modellerini koru",
  "Codex's original list (GPT models) stays on your ChatGPT session; the custom models you select are appended.":
    "Codex’in orijinal listesi (GPT modelleri) ChatGPT oturumunuzla kalır; seçilen özel modeller sona eklenir.",
  "Official Claude models stay on your own account; the custom models you select are appended.":
    "Resmî Claude modelleri kendi hesabınızla kalır; seçilen özel modeller sona eklenir.",
  "To append to the original list, select at least one model from outside":
    "Orijinal listenin altına eklemek için Yönlendirme ekranında",
  "on the Routing screen (for example EVREN).":
    "dışından (ör. EVREN) en az bir model seçin.",
  "Fall back to another model": "Başka bir modele geç",
  "If the selected provider is unreachable or rate-limited, the gateway retries the same request with the route's other models. Streaming requests only switch before the first token arrives.":
    "Seçili sağlayıcıya ulaşılamazsa veya hız sınırına takılırsa gateway aynı isteği rotanın diğer modelleriyle yeniden dener. Akış istekleri yalnızca ilk token gelmeden önce geçiş yapabilir.",
  "Before transfer": "Aktarım öncesi",
  "The config is backed up, writes are atomic and an error rolls back automatically.":
    "Config yedeklenir, yazma atomiktir ve hata olursa otomatik geri alınır.",
  "After transfer": "Aktarım sonrası",
  "The Turnout Agent restarts; the editor reads the new route through the gateway.":
    "Turnout Agent yeniden başlatılır; editör, gateway üzerinden yeni rotayı okur.",
  Rollback: "Geri alma",
  "restores the shared config from the last backup.":
    "paylaşılan config’i son yedeğe döndürür.",

  // Bağlantılar
  "Official OAuth accounts": "Resmî OAuth hesapları",
  "Codex and Claude Code use their own secure sessions; Turnout never copies the token.":
    "Codex ve Claude Code kendi güvenli oturumlarını kullanır; Turnout tokenı kopyalamaz.",
  "Saved providers": "Kayıtlı sağlayıcılar",
  "Only connections with a key can be used for routing.":
    "Yönlendirmede yalnızca anahtarı bulunan bağlantılar kullanılabilir.",
  "Claude Code only": "Yalnızca Claude Code",
  "Add API key": "API anahtarı ekle",
  "Continue with ChatGPT": "ChatGPT ile devam et",
  "Connect with account": "Hesapla bağlan",
  "Waiting for browser…": "Tarayıcı bekleniyor…",
  "Key ready": "Anahtar hazır",
  "No key": "Anahtar yok",
  Remove: "Kaldır",
  "Add provider": "Sağlayıcı ekle",
  "Pick a ready-made service or enter an OpenAI/Anthropic compatible endpoint.":
    "Hazır servis seçin veya OpenAI/Anthropic uyumlu bir uç nokta girin.",
  "Custom provider": "Özel sağlayıcı",
  Identity: "Kimlik",
  "API base URL": "API taban adresi",
  "Ready-made service address will be used": "Hazır servis adresi kullanılacak",
  Protocol: "Protokol",
  "API key": "API anahtarı",
  "The key is written only to the data folder": "Anahtar yalnızca veri klasörüne yazılır",
  "Save connection": "Bağlantıyı kaydet",
  "The key is never written to browser storage, the Turnout settings file or the logs.":
    "Anahtar tarayıcı deposuna, Turnout ayar dosyasına veya loglara yazılmaz.",
  "Where it is written": "Nereye yazılır",
  "Turnout data folder/credentials.json": "Turnout veri klasörü/credentials.json",
  "Reading connections…": "Bağlantılar okunuyor…",
  "No provider added yet.": "Henüz bir sağlayıcı eklenmedi.",
  "Claude subscription is Claude Code only.": "Claude aboneliği Claude Code’a özeldir.",
  "Anthropic does not allow Pro/Max OAuth tokens in third-party apps; Turnout never copies that token. To use Claude models in Codex or OpenCode, add a separately billed Anthropic API key.":
    "Anthropic, Pro/Max OAuth tokenlarının üçüncü taraf uygulamalarda kullanılmasına izin vermez; Turnout bu tokenı kopyalamaz. Claude modellerini Codex veya OpenCode’da kullanmak için ayrı faturalanan bir Anthropic API anahtarı ekleyin.",
  "The subscription only works inside Claude Code. Codex, OpenCode and the gateway need an API key:":
    "Abonelik yalnızca Claude Code içinde çalışır. Codex, OpenCode ve gateway için API anahtarı gerekir:",
  "Codex connected · separate authorization needed for Claude":
    "Codex bağlı · Claude için ayrı yetki gerekli",

  // Profiller sayfası
  "Launch profiles": "Başlatma profilleri",
  "A profile is injected only into the process started with it; the shared config is not touched.":
    "Profil yalnızca kendisiyle başlatılan sürece enjekte edilir; paylaşılan config değişmez.",
  "Applied route": "Uygulanan rota",
  "No applied route": "Uygulanan rota yok",
  "No saved profiles": "Kayıtlı profil yok",
  "models": "model",
  "via gateway": "gateway üzerinden",
  "direct": "doğrudan",
  "hybrid": "hibrit",
  "Copy command": "Komutu kopyala",
  "Copied": "Kopyalandı",
  "Refresh": "Yenile",
  "Save this route as a profile": "Bu rotayı profil olarak kaydet",
  "Profiles are read from": "Profiller okunuyor:",

  // Günlükler sayfası
  Requests: "İstekler",
  "What the gateway actually served: model, provider, duration and tokens. Prompts and replies are never recorded.":
    "Gateway'in gerçekte neyi sunduğu: model, sağlayıcı, süre ve token. İstemler ve yanıtlar hiçbir zaman kaydedilmez.",
  "requests": "istek",
  "input tokens": "giriş token",
  "output tokens": "çıkış token",
  "fallbacks": "yedek geçiş",
  "fallback": "yedek",
  "failed": "hatalı",
  "No requests yet": "Henüz istek yok",
  "Served by": "Sunan",
  Duration: "Süre",
  Tokens: "Token",
  "to first token": "ilk token’a",
  "Agent log": "Agent günlüğü",
  "One line per started route; the only record of what the background service served.":
    "Başlatılan her rota için bir satır; arka plan servisinin neyi sunduğunun tek kaydı.",
  "Backups": "Yedekler",
  "Every apply takes a backup; rollback restores these files.":
    "Her uygulama bir yedek alır; geri alma bu dosyaları geri yükler.",
  "Backup": "Yedek",
  "Label": "Etiket",
  "Files": "Dosyalar",
  "Date": "Tarih",
  "Restore this backup": "Bu yedeği geri yükle",
  "No backups yet": "Henüz yedek yok",
  "No log lines yet": "Henüz günlük satırı yok",
  "Loading…": "Yükleniyor…",
  "Could not read": "Okunamadı",
  "Restore the last applied plan? The shared config is restored from the last backup.":
    "Son uygulanan plan geri alınsın mı? Paylaşılan config son yedekten geri yüklenir.",

  // Komut paleti
  Commands: "Komutlar",
  "Test the connection": "Bağlantıyı test et",
  "Send a real request to the selected route": "Seçili rotaya gerçek bir istek gönder",
  "Apply the plan": "Planı uygula",
  "Show the diff, back up, write": "Diff’i göster, yedekle, yaz",
  "Revert the last applied plan": "Son uygulanan planı geri çevir",
  "navigate": "gez",
  "select": "seç",
  "close": "kapat",
  "Close search": "Aramayı kapat",

  // Ayarlar
  Language: "Dil",
  "Interface language": "Arayüz dili",
  "The interface language is remembered on this computer.":
    "Arayüz dili bu bilgisayarda hatırlanır.",
  "Data folder": "Veri klasörü",
  "Turnout Agent": "Turnout Agent",
  "Running": "Çalışıyor",
  "Stopped": "Kapalı",
  "Restore the last applied plan": "Son uygulanan planı geri yükle",
  "Version": "Sürüm",
  "Open the terminal interface": "Terminal arayüzünü aç",
  "Full-screen CLI": "Tam ekran CLI",
  "OpenAI coding tool": "OpenAI kodlama aracı",
  "Open-source coding tool": "Açık kaynak kodlama aracı",
  "Anthropic coding tool": "Anthropic kodlama aracı",
  "connection saved securely": "bağlantı güvenli şekilde kaydedildi",
  "Could not save": "Kaydedilemedi",
  "connection and its stored key will be removed. Continue?": "bağlantısı ve saklanan anahtarı kaldırılsın mı?",
  "connection removed": "bağlantı kaldırıldı",
  "Could not remove": "Kaldırılamadı",
  "official sign-in page opened": "resmî giriş sayfası açıldı",
  "OAuth connection ready": "OAuth bağlantısı hazır",
  "sign-in not finished yet": "giriş henüz tamamlanmadı",
  "Could not start OAuth": "OAuth başlatılamadı",
  "Could not read connections": "Bağlantılar okunamadı",
  "OAuth session is no longer active": "OAuth oturumu artık aktif değil",
  "Official session verified": "Resmî oturum doğrulandı",
  "transferred to VS Code; original list kept": "VS Code’a aktarıldı; orijinal liste korundu",
  "official OAuth session ready; config not changed": "resmî OAuth oturumu hazır; config değiştirilmedi",
  "official OAuth route applied to VS Code": "resmî OAuth rotası VS Code’a uygulandı",
  "models transferred to the VS Code extension": "model VS Code eklentisine aktarıldı",
  "Could not apply": "Uygulanamadı",
  "Rolled back": "Geri alındı",
  "Last plan rolled back": "Son plan geri alındı",
  "Could not roll back": "Geri alınamadı",
  "Default routes restored": "Varsayılan yönlendirmeler geri yüklendi",
  "OpenAI ready": "OpenAI hazır",
  "OAuth connected": "OAuth bağlı",

  // Tek kaynak: yönlendirme CLI'dan okunur
  "Catalog": "Katalog",
  "routed": "rotada",
  "Catalog models": "Katalogdaki modeller",
  "Routed but not in the catalog": "Rotada ama katalogda yok",
  "Reading…": "Okunuyor…",
  "unknown": "bilinmiyor",
  "Refresh": "Yenile",
  "Source": "Kaynak",
  "gateway route (no profile yet)": "gateway rotası (henüz profil yok)",
  "No route yet": "Henüz rota yok",
  "Could not read the routing state": "Yönlendirme durumu okunamadı",
  "Run `turnout profiles` in a terminal; if it fails, restart the gateway with `turnout gateway start`.":
    "Terminalde `turnout profiles` çalıştırın; hata sürerse gateway'i `turnout gateway start` ile yeniden başlatın.",
  "Try again": "Yeniden dene",
  "Testing and saving are off until the routing state is read.": "Yönlendirme durumu okunana kadar test ve kaydetme kapalı.",
  "unsaved change": "değişiklik kaydedilmedi",
  "unsaved changes": "değişiklik kaydedilmedi",
  "The saved profile is unchanged until you save.": "Kaydedene kadar kayıtlı profil değişmez.",
  "Fallback order": "Fallback sırası",
  "read-only": "salt okunur",
  "default": "varsayılan",
  "Chain for a default request. The requested model is tried first; on overload, rate limit or an unreachable provider the route's other models are tried in route order. Once a response starts streaming it is not retried.":
    "Varsayılan istek için zincir. Önce istenen model denenir; aşırı yük, hız sınırı veya erişilemezlikte rotadaki diğer modeller rota sırasıyla denenir. Yanıt akmaya başladıktan sonra yeniden deneme yapılmaz.",
  "Fallback is off for this route; only the requested model is used.": "Bu rotada fallback kapalı; yalnızca istenen model kullanılır.",
  "more models": "model daha göster",
  "Show fewer": "Daha az göster",
  "VS Code / gateway": "VS Code / gateway",
  "Testing…": "Test ediliyor…",
  "Connection test is running": "Bağlantı test ediliyor",
  "saved result": "kayıtlı sonuç",
  "What to do: check the provider key on the Connections page, then test again.":
    "Ne yapmalı: Bağlantılar sayfasında sağlayıcı anahtarını kontrol edin, sonra tekrar test edin.",
};

export const translations = { tr, en: {} };

export function translate(language, key) {
  if (language === "en") return key;
  return translations[language]?.[key] ?? key;
}
