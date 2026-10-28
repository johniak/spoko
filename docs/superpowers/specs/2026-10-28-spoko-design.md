# spoko — dokument projektowy

Data: 2026-10-28
Status: szkic do akceptacji

Legenda: **[decyzja]** — ustalone z autorem w rozmowie. **[moja decyzja]** — decyzja projektanta, do sprawdzenia przez autora. **[otwarte]** — wymaga decyzji autora.

---

## 1. Po co to jest

Developer pracuje na kilku komputerach (Mac, Linux, Windows). Przesiadka z jednego na drugi dziś oznacza: commit „wip” albo stash, push, pull, przekopiowanie `.env`, odtworzenie bazy w Dockerze, utratę kontekstu rozmów z Claude Code. Dropbox i podobne narzędzia nie nadają się do kodu: przy rozjeździe zostawiają „conflicted copy” zamiast merga, synchronizują `node_modules` i inne śmieci, nie rozumieją gita.

**spoko** synchronizuje *stan pracy* developera między jego komputerami — w momencie, gdy jeszcze nic nie jest zacommitowane. Siadasz do drugiego komputera i jest tam dokładnie to, co zostawiłeś na pierwszym: pliki, gałąź, commity, staged zmiany, `.env`, czaty Claude Code, (na żądanie) dane bazy z Dockera.

W 95% przypadków ma być przezroczysty: każda zmiana pliku jest natychmiast wypychana na serwer, a pozostałe komputery od razu ją pobierają. W pozostałych 5% (praca równoległa, offline) działa merge w stylu gita — oparty na gicie, nie na własnym algorytmie.

To nie jest zamiennik gita. Git dalej służy do historii, gałęzi i współpracy z innymi; spoko przenosi stan *pomiędzy* commitami.

## 2. Cele, nie-cele, anty-cele

### Cele (wszystkie w MVP) [decyzja]
1. Natychmiastowa synchronizacja plików projektu między komputerami jednego developera.
2. Pełne lustro stanu gita: gałąź (HEAD), commity, lokalne gałęzie, staged zmiany (index), stashe, hooki, lokalny config.
3. Działanie także dla folderów **bez** gita.
4. Merge przy rozjeździe oparty na 3-way merge gita.
5. Pliki duże / binarne / LFS — bez mergowania.
6. `.spokoconfig`: reguły co synchronizować (dziedziczenie z `.gitignore`, wyjątki, dodatkowe ignory), pilnowanie lock-files z hookami reinstalacji, Docker volumes, zewnętrzne ścieżki.
7. Synchronizacja `.env` i innych lokalnych plików ignorowanych przez gita (przez wyjątki w configu).
8. Synchronizacja czatów, pamięci i planów Claude Code dla projektu, z mapowaniem ścieżek między systemami.
9. Synchronizacja Docker volumes (Postgres i inne), statyk/mediów deweloperskich.
10. Wieloplatformowość: macOS, Linux, Windows — z obsługą wszystkich różnic, które mogą zepsuć dane.
11. Szyfrowanie end-to-end: serwer nie może odczytać kodu ani sekretów.
12. Open source (klient + serwer), łatwy self-hosting. Usługa hostowana z wieloma kontami (model git/GitHub) to **etap 2** [decyzja] — w MVP model danych serwera ma już `account_id` wszędzie, ale bez rejestracji, weryfikacji maili, resetu haseł, billingu i operacji publicznej usługi.
13. Mały i bardzo szybki — Rust.

### Nie-cele
- Współpraca w czasie rzeczywistym między **różnymi** osobami na jednym projekcie (to jest narzędzie dla jednego developera i jego komputerów) [moja decyzja, wynika z założenia autora].
- Zastąpienie gita, pushowanie do origin — spoko **nigdy** sam nie pushuje do zdalnych repo gita [moja decyzja].
- Synchronizacja `node_modules`, `target/`, `.venv` i innych artefaktów odtwarzalnych z lock-files [decyzja].
- Płatności / billing w usłudze hostowanej — po MVP [moja decyzja].
- Pełna obsługa `git worktree` i submodułów w MVP — w MVP wykrywanie i ostrzeżenie (patrz 6.6) [moja decyzja].

### Anty-cele (porażka, nawet jeśli „działa”)
- Zepsute repo gita u użytkownika. Kiedykolwiek.
- Utrata danych: każde nadpisanie musi być odwracalne.
- Pliki „conflicted copy” zaśmiecające projekt.
- Znaczniki konfliktu wstawione w plik, który właśnie edytujesz.
- Ślady spoko w repo gita użytkownika (ukryte refy, wpisy w config, coś w `git log` / `git branch -a`).
- Serwer (lub ktoś, kto go przejmie) zdolny odczytać kod lub `.env`.
- Narzędzie, które trzeba ciągle obsługiwać: hasło przy każdym starcie, ręczne potwierdzanie każdej zmiany.
- Ciężki, wolny daemon zjadający CPU/baterię w tle.

## 3. Ograniczenia

- Język: Rust — klient i serwer [decyzja].
- Systemy: macOS (ARM i Intel), Linux (x86-64, ARM64), Windows (x86-64; WSL traktowane jak Linux) [decyzja].
- Na różnych komputerach projekt leży pod **różnymi ścieżkami** i na różnych systemach [decyzja].
- Serwer nie widzi treści — więc **nie może** mergować. Cała logika merga jest po stronie klienta.
- Licencja: **[otwarte]** — patrz sekcja 14.

## 4. Architektura w skrócie

```
 komputer A                                         komputer B
┌──────────────────────────┐                      ┌──────────────────────────┐
│ projekt/ (pliki, .git)   │                      │ projekt/ (pliki, .git)   │
│      ▲  watcher          │                      │      ▲                   │
│      │                   │                      │      │                   │
│ spoko daemon             │   szyfrowane bloby   │ spoko daemon             │
│  ├ wewnętrzne repo git   │ ───────────────────► │  ├ wewnętrzne repo git   │
│  │  (~/.spoko/…)         │ ◄─────────────────── │  │  (~/.spoko/…)         │
│  ├ szyfrowanie           │   powiadomienia (WS) │  ├ szyfrowanie           │
│  └ merge (3-way, git)    │                      │  └ merge (3-way, git)    │
│ spoko CLI · tray         │                      │ spoko CLI · tray         │
└──────────────────────────┘                      └──────────────────────────┘
                     ▲                  ▲
                     └──── spoko-server ┘
             (konta, urządzenia, zaszyfrowany magazyn,
              wskaźnik „head” projektu z CAS, powiadomienia)
```

Komponenty:
- **`spoko`** — jedna binarka klienta: CLI + daemon (`spoko daemon`) [moja decyzja: jedna binarka, mniej do instalowania].
- **Tray / ikona w pasku menu** + natywne powiadomienia systemowe — status (zsynchronizowano / synchronizuję / konflikt / offline), szybkie akcje [moja decyzja: w MVP minimalny tray; w rozmowie autor nie wybrał formy, a powiadomienia o konfliktach i pytania o volumeny tego wymagają].
- **`spoko-server`** — jedna binarka serwera.

## 5. Model danych klienta: wewnętrzne repo spoko

Każdy projekt ma wewnętrzne repo git spoko poza katalogiem projektu, np. `~/.spoko/projects/<project-id>/store.git` (bare). Używane wyłącznie przez spoko jako silnik: magazyn obiektów, snapshoty, wspólny przodek, 3-way merge. Użytkownik nie musi o nim wiedzieć. [decyzja: „git ma być niezależny od aplikacji”, „kod nawet nie musi być w gicie”]

**Snapshot** = commit w wewnętrznym repo, którego drzewo opisuje cały stan pracy. Proponowany układ [moja decyzja, szczegóły dla buildera]:

```
work/        pliki projektu (po filtrach z .spokoconfig), bajt w bajt
gitstate/    stan repo gita użytkownika (jeśli istnieje) — patrz sekcja 6
external/    zewnętrzne ścieżki (np. claude/ — czaty Claude Code), patrz sekcja 9
meta/        metadane: pochodzenie snapshotu (urządzenie, czas), otwarte konflikty
             (bit wykonywalny i symlinki zapisywane natywnie w trybach drzewa git: 100755 / 120000)
```

Historia snapshotów daje za darmo: wspólnego przodka do merga, możliwość przywrócenia dowolnego pliku z dowolnego momentu (`spoko log`, `spoko restore`), odwracalność każdego nadpisania.

Retencja historii: [moja decyzja] w MVP historia jest trzymana w całości (self-host, jeden użytkownik). Rozrzedzanie (np. pełna z 7 dni, godzinowa do 30 dni) wymaga GC po stronie klienta, bo serwer nie widzi powiązań między obiektami — etap 2, razem z kwotami w usłudze hostowanej.

### Cykl życia zmiany (typowy przypadek, 95%)
1. Watcher wykrywa zmianę → debounce (~100–300 ms) → snapshot (tylko zmienione pliki są hashowane — cache stat-ów jak index gita).
2. Nowe obiekty → szyfrowanie → upload.
3. Klient przesuwa wskaźnik `head` projektu na serwerze operacją **compare-and-swap** (oczekiwany poprzedni head = snapshot, na którym bazował).
4. Serwer rozsyła powiadomienie do pozostałych urządzeń.
5. Komputer B pobiera snapshot. Jeśli B nie ma lokalnych zmian od poprzedniego snapshotu — fast-forward: zapis plików atomowo (temp + rename), zastosowanie stanu gita, oznaczenie własnych zapisów, żeby watcher nie odbił ich z powrotem.

### Rozjazd (5%)
Jeśli CAS się nie powiedzie (ktoś inny przesunął head) albo B ma lokalne zmiany, gdy przychodzi nowy snapshot:
1. Klient pobiera nowy head, znajduje wspólnego przodka w historii snapshotów.
2. 3-way merge gita (biblioteka git w Ruście, np. `gix`; fallback do `git merge-tree` dopuszczalny, jeśli `gix` nie pokrywa danego przypadku — decyzja buildera).
3. Jeśli merge czysty — snapshot merge’owy, CAS, koniec. Użytkownik niczego nie widzi.
4. Jeśli konflikt — patrz sekcja 7.

Serwer nigdy nie merguje — nie może, bo nie widzi treści [konsekwencja E2E].

## 6. Git użytkownika

### 6.1 Zasada „git niezależny” [decyzja]
- spoko **nie dodaje niczego swojego** do `.git` użytkownika: żadnych ukrytych refów, kluczy w config, hooków, plików. Po odinstalowaniu spoko repo wygląda tak, jakby spoko nigdy nie było.
- spoko zapisuje do `.git` na komputerze B wyłącznie **dane użytkownika pochodzące z komputera A**, w standardowych strukturach gita.
- Projekt bez `.git` działa normalnie; sekcja `gitstate/` jest wtedy pusta.

### 6.2 Co ma być lustrem [decyzja]
Przykład autora: na A jestem na gałęzi `johniak`, commituję i przełączam się na `dzban` → na B widzę, że jestem na `dzban`, a commit jest w historii `johniak`. Zwykłe `git status` / `git log` na B pokazują to samo co na A.

Zakres: HEAD (gałąź lub detached), wszystkie lokalne refy (gałęzie, tagi), remote-tracking refy, obiekty osiągalne z nich, **index** (staged zmiany), stashe, `.git/config`, `.git/hooks/`, `.git/info/exclude`, obiekty git-lfs (`.git/lfs/objects`, jako pliki binarne).

### 6.3 Jak — replikacja semantyczna, nie kopiowanie plików `.git` [moja decyzja]
Kopiowanie katalogu `.git` jak zwykłych plików prowadzi do zepsutych repo (locki, index z danymi stat konkretnej maszyny, packi przepisywane przez `gc`, absolutne ścieżki). Zamiast tego:
- Na A daemon czyta stan repo biblioteką git: HEAD, mapę refów, drzewo indexu (`write-tree`), listę stashy, wymienione pliki konfiguracyjne. Obiekty gita użytkownika trafiają do wewnętrznego repo spoko (to te same obiekty git — deduplikują się z plikami z `work/`).
- Na B daemon zapisuje brakujące obiekty do `.git` B, ustawia refy i HEAD, odtwarza index z drzewa, a pliki robocze odtwarza z `work/`.
- Operacje na `.git` tylko wtedy, gdy git jest bezczynny (brak `*.lock` w `.git`), z krótkim odczekaniem po ostatniej operacji gita. Odczyt stanu też dopiero po ustaniu operacji (np. w trakcie rebase czekamy).
- Stan „w trakcie” (rebase, merge, cherry-pick w toku — `MERGE_HEAD`, `rebase-merge/` itd.): [moja decyzja] w MVP nie replikujemy go; daemon wstrzymuje synchronizację sekcji `gitstate/` do czasu zakończenia operacji i pokazuje to w statusie. Pliki robocze synchronizują się dalej.
- `.git/config` zawiera czasem ścieżki absolutne / ustawienia zależne od systemu (`core.filemode`, `core.autocrlf`, `core.worktree`, credential helpers) — builder ustala listę kluczy, których nie przenosimy albo które przepisujemy.

Uwaga: w rozmowie wcześniej padły dwie propozycje, które zostały **zastąpione**: (a) ukryte refy `refs/spoko/<komputer>` w `.git` użytkownika — odrzucone przez autora; (b) serwer jako gołe repo git z natywnym protokołem git — niezgodne z szyfrowaniem E2E (patrz sekcja 11).

### 6.4 Rozjazd stanu gita [moja decyzja]
- Refy: per ref 3-way względem przodka. Ruszył tylko jeden bok → bierzemy go. Oba boki przesunęły tę samą gałąź na różne commity (commitowałeś offline na obu) → gałąź dostaje wersję nowszą, druga zostaje jako zwykła gałąź `<nazwa>-<nazwa-urządzenia>` + powiadomienie. (To jedyny przypadek, w którym spoko tworzy gałąź — jest to dana użytkownika, nie wewnętrzna struktura spoko.)
- HEAD: nowszy wygrywa.
- Index: nowszy wygrywa, starszy zostaje w historii snapshotów.
- Pliki robocze: 3-way merge (sekcja 7). Jeśli A przełączył gałąź, a B miał niezacommitowane zmiany na poprzedniej — merge przenosi zmiany B na nowy stan, jak `git checkout` z niezacommitowanymi zmianami.

### 6.4a Zakres historii [moja decyzja]
spoko przenosi **pełne lustro**: wszystkie obiekty osiągalne z replikowanych refów, przez własny zaszyfrowany magazyn. Działa bez dostępu do origin i bez poświadczeń gita, a obiekty deduplikują się z plikami z `work/`. Optymalizacja „resztę pobierz z origin” — po MVP (istotna w etapie 2 ze względu na koszty miejsca).

### 6.5 LFS
Pliki śledzone przez git-lfs (`filter=lfs` w `.gitattributes`) traktowane jak klasa „binarna” (sekcja 8). Obiekty w `.git/lfs/objects` przenoszone jako binarne, content-addressed — nie konfliktują.

### 6.6 Worktrees i submoduły [moja decyzja]
`git worktree` i submoduły zawierają ścieżki absolutne / zagnieżdżone repo. W MVP: wykrycie, ostrzeżenie w `spoko status`, synchronizacja plików roboczych submodułu jako zwykłych plików, bez replikacji ich stanu gita. Pełna obsługa po MVP.

## 7. Konflikty [decyzja]

1. Najpierw automatyczny 3-way merge gita — w większości przypadków przejdzie bez wiedzy użytkownika.
2. Jeśli konflikt: spoko **nie rusza pliku na komputerze, na którym aktualnie pracujesz** (żadnych znaczników `<<<<<<<` w trakcie pisania). Wersja z drugiego komputera jest odłożona (w historii snapshotów, nie jako plik w projekcie), pojawia się powiadomienie i status „konflikt” w trayu i `spoko status`.
3. `spoko resolve` otwiera 3-way merge w narzędziu użytkownika — to, co ma ustawione w gicie (`merge.tool`), np. merge editor VS Code; jeśli nic nie ustawione — sensowny domyślny wybór lub znaczniki w pliku na żądanie.
4. Po rozwiązaniu synchronizacja jedzie dalej. Do czasu rozwiązania pozostałe pliki synchronizują się normalnie; konfliktujące pliki są wstrzymane.

„Komputer, na którym aktualnie pracujesz” = ten z najświeższą lokalną aktywnością w projekcie [moja decyzja]. Na drugim (nieaktywnym) komputerze w razie konfliktu też nic się nie nadpisuje bez śladu.

### Stan synchronizacji przy otwartym konflikcie [moja decyzja]
- Jeden head na serwerze wystarcza. Aktywny komputer wypycha snapshot merge’owy, w którym konfliktujący plik ma **jego** wersję, a `meta/conflicts` zapisuje: ścieżkę, przodka, wersję drugiego komputera (identyfikatory obiektów).
- Nieaktywny komputer pobiera ten head: jego własna wersja pliku jest już w historii snapshotów (i w `meta/conflicts`), na dysk trafia wersja aktywnego. Oba komputery mają ten sam stan plików i tę samą listę konfliktów.
- `spoko resolve` działa na dowolnym komputerze — czyta trzy wersje z `meta/conflicts`, wynik to zwykła zmiana pliku, która czyści wpis konfliktu w kolejnym snapshocie.
- Do czasu rozwiązania dalsze edycje konfliktującego pliku na aktywnym komputerze synchronizują się normalnie; wpis konfliktu zostaje.

### Czysty merge pliku edytowanego na obu komputerach [moja decyzja]
Czysty 3-way merge jest zapisywany do pliku po cichu, także jeśli plik był lokalnie zmieniony (atomowo, z poprzednią wersją w snapshocie). To zgodne z założeniem „przezroczyste w 95%”. Edytory wykrywają zewnętrzną zmianę i przeładowują plik albo ostrzegają, jeśli mają niezapisany bufor; spoko nie ma wglądu w niezapisane bufory edytorów.

### Dołączenie komputera, który już ma kopię projektu [moja decyzja]
`spoko clone <projekt> <path>` na niepustym katalogu (np. istniejący klon repo z własnymi niezacommitowanymi zmianami):
1. spoko robi lokalny snapshot istniejącego stanu („stan sprzed dołączenia”) — nic nie ginie.
2. Pliki identyczne po obu stronach — bez akcji. Obiekty gita wspólne z projektem — deduplikowane.
3. Źródłem prawdy jest stan projektu na serwerze: na dysk trafia jego stan (pliki, gałąź, index), a różnice lokalne zostają w snapshocie „sprzed dołączenia”.
4. `spoko status` i powiadomienie pokazują listę plików, które się różniły; `spoko restore --from-join [plik]` przywraca lokalną wersję (staje się zwykłą zmianą, która synchronizuje się dalej). Lokalne gałęzie/commity, których nie ma w projekcie, są zachowane w `.git` (dodawane, nie usuwane).
Uzasadnienie: bez wspólnego przodka nie ma bezpiecznego 3-way merge; wariant „jedno źródło + nic nie ginie” jest prosty i bezpieczny. Łączenie obu stanów przez konflikty — po MVP, jeśli okaże się potrzebne.

## 8. Klasy plików

| Klasa | Przykłady | Zachowanie |
|---|---|---|
| tekst / kod | źródła, configi, `.env` | sync natychmiast, 3-way merge |
| binarne / duże / LFS | obrazki, uploady, pliki LFS, `media/` | sync natychmiast (lub z opóźnieniem dla dużych), **bez merga**: nowsza wersja wygrywa, starsza w historii, powiadomienie [decyzja] |
| ignorowane | `node_modules`, `target/`, `.venv`, cache | nie synchronizowane [decyzja] |
| pilnowane (watch) | `package-lock.json`, `Cargo.lock`, `uv.lock`, `.tool-versions`, `mise.toml`, `.nvmrc`, `rust-toolchain.toml` | synchronizowane jak tekst + po przyjściu zmiany uruchamia hook (sekcja 10) [decyzja] |
| stan gita | `.git` | replikacja semantyczna (sekcja 6) |
| zewnętrzne | czaty Claude Code | sekcja 9 |
| Docker volumes | `pg_data` | na żądanie / przy przesiadce (sekcja 12) |

Wykrywanie binarnych: wzorce w configu, `filter=lfs`, heurystyka zawartości (jak git), próg rozmiaru [moja decyzja: np. > 10 MB traktowane jak binarne niezależnie od treści].

Domyślnie zawsze ignorowane (do nadpisania w configu): `.DS_Store`, `Thumbs.db`, `.idea/workspace.xml` [moja decyzja].

## 9. Zewnętrzne ścieżki — czaty Claude Code [decyzja]

Mechanizm ogólny: „zewnętrzne ścieżki z mapowaniem” — katalogi poza projektem, przypisane do projektu, ze ścieżką zależną od lokalnej lokalizacji projektu. Claude Code jest wbudowanym presetem; Cursor / Codex jako kolejne presety bez zmiany architektury [moja decyzja: w MVP preset Claude Code obowiązkowy, inne jeśli starczy czasu].

Preset Claude Code:
- Lokalizacja: `~/.claude/projects/<zakodowana-ścieżka-projektu>/`. Na macOS zaobserwowane kodowanie: `/Users/johniak/Projects/spoko` → `-Users-johniak-Projects-spoko` (separatory → `-`). **Builder weryfikuje** dokładne reguły kodowania na Linuksie i Windows (dyski `C:`, `\`, kropki itd.).
- Zawartość: sesje `*.jsonl`, `memory/`, plany i inne podkatalogi projektu. Projektowe `.claude/settings.local.json` jest w katalogu projektu — przechodzi przez wyjątki w configu.
- Mapowanie ścieżek: projekt ma globalne ID; każdy komputer zna swoją lokalną ścieżkę. Przy zapisie na B katalog dostaje nazwę z kodowania lokalnej ścieżki B, a ścieżki absolutne w treści JSONL (np. pole `cwd`, ścieżki plików w wywołaniach narzędzi) są przepisywane z prefiksu A na prefiks B (z uwzględnieniem separatorów Windows). Builder ustala, które pola przepisywać.
- Merge: sesje są dopisywane na końcu (append-only) — merge = suma linii, deduplikacja po identyfikatorach wiadomości (`uuid`). Pliki `memory/` — jak zwykły tekst (3-way).

## 10. `.spokoconfig` [decyzja: nazwa i jeden plik]

Plik TOML w katalogu głównym projektu, synchronizowany jak zwykły plik (autor sam decyduje, czy go commitować do gita). Szkic schematu [moja decyzja, nazwy do dopracowania]:

```toml
[ignore]
inherit_gitignore = true            # dziedzicz reguły z .gitignore (wszystkich poziomów)
include = [".env*", "config/local.*", "docker-compose.override.yml", ".claude/settings.local.json"]
exclude = ["*.log", "tmp/"]

[binary]
patterns = ["media/**", "uploads/**", "*.psd"]
size_threshold = "10MB"

[[watch]]
files = ["package-lock.json"]
run = "npm ci"
mode = "ask"                        # ask | auto | notify
# opcjonalnie per system:
# run_windows = "npm.cmd ci"

[[watch]]
files = ["Cargo.lock"]
run = "cargo fetch"
mode = "notify"

[[watch]]
files = [".tool-versions", "mise.toml"]
run = "mise install"
mode = "ask"

[[docker.volume]]
name = "pg_data"                    # nazwa logiczna z docker-compose, bez prefiksu projektu
mode = "raw"                        # raw | dump
stop_container = true
# dla mode = "dump":
# dump = "pg_dump -Fc -U postgres app"
# restore = "pg_restore -c -U postgres -d app"

[external.claude_code]
enabled = true
```

Hooki `watch`: uruchamiane na komputerze, który **otrzymał** zmianę. Domyślnie `ask` — powiadomienie z przyciskiem; `auto` — od razu; `notify` — tylko informacja. Hook zmusza do reinstalacji — dopóki nie wykonany, `spoko status` pokazuje ostrzeżenie [decyzja: „przymusiło do reinstalacji”; forma — moja decyzja].

Konfiguracja per komputer (`~/.spoko/config.toml`, niesynchronizowana): adres serwera, mapowanie project-ID → lokalna ścieżka, nadpisania hooków, lokalne ustawienia trayu/autostartu.

Walidacja: `spoko config check` pokazuje, co jest synchronizowane, ignorowane, pilnowane — z rozmiarami.

## 11. Szyfrowanie i klucze [decyzja]

Model zagrożeń: chronimy przed serwerem i każdym, kto go przejmie. **Nie** chronimy przed kimś z dostępem do komputera — ma i tak dostęp do kodu [decyzja autora].

- Klucz konta / projektu: symetryczny (np. XChaCha20-Poly1305). Każde urządzenie: para kluczy (X25519). Klucz projektu przechowywany na serwerze w kopii zaszyfrowanej osobno dla każdego urządzenia.
- Lokalnie: **plik z kluczem** w `~/.spoko/`, uprawnienia tylko dla użytkownika (Unix 0600, Windows ACL). Bez systemowych sejfów. Hasło nigdy przy starcie [decyzja].
- Dodanie komputera: parowanie kodem — nowy komputer pokazuje krótki kod, użytkownik zatwierdza go na już podłączonym, ten przekazuje klucz [moja decyzja, autor nie zakwestionował]. Alternatywa: ręczne skopiowanie pliku klucza.
- Klucz odzyskiwania: generowany raz przy zakładaniu konta, do zapisania — gdy żaden stary komputer nie jest dostępny [moja decyzja].
- Usunięcie urządzenia: rotacja klucza projektu; nowe dane niedostępne dla starego urządzenia.
- Co widzi serwer: tylko nieprzezroczyste bloby, ich rozmiary i czasy. Nazwy plików, struktura, treść — zaszyfrowane (drzewa są zaszyfrowanymi obiektami). Identyfikatory blobów to klucz-owane hashe (np. HMAC), nie surowe hashe git — żeby nie dało się sprawdzić „czy masz plik X”.
- Bez własnej kryptografii — tylko sprawdzone biblioteki.

Konsekwencja dla transferu: serwer nie może mówić protokołem git ani robić delt po treści. Klient sam pakuje obiekty (delty / chunking z deduplikacją, np. FastCDC dla dużych plików), szyfruje i wysyła. Format i parametry — decyzja buildera, przy wymaganiu: mała zmiana w dużym repo = mały transfer; duży plik zmieniony w części = transfer tylko zmienionych chunków; wznowienie przerwanego uploadu.

## 12. Docker volumes i statyki [decyzja]

- Dostęp do volumenów **wyłącznie przez Docker API** (helper container z zamontowanym volumenem, strumień tar) — na Macu i Windowsie volumeny są w VM Docker Desktop, nie na hoście [moja decyzja].
- Volumeny identyfikowane **nazwą logiczną** z configu; prefiks projektu compose (zależny od nazwy katalogu, który różni się między komputerami) rozwiązywany lokalnie.
- Tryb `raw`: zatrzymanie kontenera (`stop_container`) → kopia → wznowienie. Obok danych zapisywane: digest obrazu, wersja major bazy (jeśli wykrywalna), architektura, rodzina libc obrazu. Przy niezgodności po drugiej stronie: ostrzeżenie i — jeśli skonfigurowany — fallback do `dump`.
- Tryb `dump`: komendy dump/restore z configu, wykonywane w kontenerze.
- Kompatybilność katalogu danych Postgresa ARM64 ↔ x86-64 dla tego samego obrazu i wersji major: **spodziewana, ale niezweryfikowana** — builder sprawdza empirycznie (Postgres, MySQL/MariaDB, Redis), a wynik ustala domyślny tryb dla każdej bazy. Znane ryzyko: różne biblioteki kolacji (np. obraz debian vs alpine) mogą po cichu psuć indeksy tekstowe — spoko ostrzega przy różnicy obrazów.
- Kiedy: **nie** automatycznie z każdą zmianą. Gdy spoko wykryje przesiadkę (pierwsza lokalna aktywność w projekcie na B po tym, jak ostatnio aktywny był A), pokazuje pytanie: „Ściągnąć stan `pg_data` z komputera A?”. Do tego ręcznie `spoko volume push/pull [name]`. Push na A przy przesiadce: [moja decyzja] A robi snapshot volumenu na żądanie B (jeśli A online) albo udostępnia ostatni wypchnięty; pytanie na B mówi, z kiedy jest stan.
- Transfer volumenów przyrostowy (chunking + deduplikacja), bo kolejne wersje bazy różnią się niewiele.
- Statyki / media deweloperskie w projekcie (`media/`, `uploads/`, fixtures) — klasa binarna (sekcja 8), synchronizowane na bieżąco.

## 13. Wieloplatformowość [decyzja]

Wymagania:
- **Wielkość liter**: `Foo.ts` i `foo.ts` na Linuksie to dwa pliki, na macOS/Windows kolizja → wykryć, nie nadpisywać, ostrzec.
- **Nazwy niedozwolone na Windows** (`CON`, `PRN`, `AUX`, `NUL`, `COM1`…, `aux.js`, znaki `<>:"|?*`, końcowe kropki/spacje) i **długie ścieżki** (> 260 znaków; używać API z długimi ścieżkami) → wykryć przed zapisem, ostrzec, nie gubić pliku (pozostaje w snapshocie).
- **Końce linii**: bajt w bajt. Wewnętrzne repo bez `autocrlf` i bez filtrów. Plik nie może „się zmieniać” przez sam przejazd przez Windows.
- **Bit wykonywalny**: przechowywany w `meta/`, odtwarzany na Unixach, zachowywany (nie gubiony) przy przejściu przez Windows.
- **Symlinki**: przechowywane jako symlinki; na Windows bez uprawnień — fallback (np. plik z celem + ostrzeżenie), decyzja buildera.
- **Unicode w nazwach**: normalizacja NFC/NFD (macOS) — ten sam plik nie może pojawić się dwa razy.
- **Watcher**: natywne mechanizmy (FSEvents, inotify, ReadDirectoryChangesW — np. biblioteka `notify`) + okresowy rescan jako siatka bezpieczeństwa (gubione zdarzenia, limity inotify).
- **Autostart daemona**: launchd (macOS), systemd user (Linux), autostart przy logowaniu (Windows).
- Ścieżki projektów różne na każdym komputerze (sekcja 9, 10).

## 14. Serwer i model hostingu [decyzja]

- Open source, jedna binarka Rusta; self-host: `docker run` lub binarka + katalog danych [moja decyzja].
- Metadane: SQLite (self-host); dla hostowanej usługi możliwość Postgresa — za warstwą abstrakcji [moja decyzja].
- Magazyn blobów: dysk lokalny w MVP; S3-kompatybilny w etapie 2 (razem z usługą hostowaną), za tym samym interfejsem [moja decyzja].
- MVP [moja decyzja, wynika z „wiele kont to etap 2”]: self-host, konto(a) zakładane komendą administracyjną serwera (`spoko-server account create`), która zwraca token startowy; urządzenia dostają długotrwałe tokeny. Schemat danych zawiera `account_id`, żeby etap 2 nie wymagał migracji architektury.
- Etap 2: rejestracja e-mail + hasło (+ ewentualnie OAuth GitHub), weryfikacja maili, reset hasła, kwoty miejsca, rate limiting, Postgres + S3, backupy, monitoring, billing. Hasło konta służy tylko do logowania — **nie** do szyfrowania danych.
- Funkcje serwera: przechowywanie zaszyfrowanych blobów, wskaźnik `head` projektu z compare-and-swap, lista snapshotów/urządzeń, rozsyłanie powiadomień (WebSocket, z fallbackiem), skrzynka do parowania urządzeń, przechowanie zaszyfrowanych kopii klucza projektu.
- Klient działa z dowolnym serwerem — adres to ustawienie (jak remote w gicie).
- Billing: po MVP.
- **Licencja [otwarte]**: MIT/Apache vs AGPL. Przy MIT/Apache każdy może zrobić z serwera konkurencyjną usługę; AGPL zmusza taką usługę do publikowania zmian (typowe w modelu „open source + hosting”). Możliwy też podział: klient MIT/Apache, serwer AGPL.

## 15. Interfejs użytkownika

CLI (szkic, nazwy do dopracowania) [moja decyzja]:
```
spoko login [server]           logowanie / rejestracja urządzenia
spoko init                     dodaj bieżący katalog jako nowy projekt
spoko clone <projekt> [path]   pobierz istniejący projekt na ten komputer
spoko status                   stan synchronizacji, konflikty, ostrzeżenia, hooki do wykonania
spoko resolve [plik]           rozwiąż konflikt w narzędziu merge
spoko log [plik]               historia snapshotów
spoko restore <plik> --at ...  przywróć wersję
spoko pause / resume           wstrzymaj synchronizację projektu
spoko volume push|pull [name]
spoko device add|approve|list|revoke
spoko config check
spoko daemon [start|stop|install]
```
Tray: status per projekt, ostatnia synchronizacja, konflikty, pytania (hooki, volumeny), pauza.
Powiadomienia systemowe: konflikt, nadpisanie binarnego, hook do wykonania, pytanie o volumen, ostrzeżenia platformowe.

## 16. Wydajność — cele [moja decyzja]

- Mała zmiana w pliku tekstowym → widoczna na drugim komputerze: < 1 s (p50) przy dobrym łączu, < 3 s (p95).
- Bezczynny daemon: ~0% CPU, < 50 MB RAM dla typowego projektu.
- Skala: repo ze 100 000 plików (po filtrach) bez degradacji; cache stat-ów, żeby nie hashować niezmienionych plików.
- Zimny start (po restarcie komputera): skan zmian w typowym projekcie < 2 s.
- Binarka klienta: rząd kilkunastu MB lub mniej.
- Pierwsza synchronizacja dużego projektu: przyrostowa, wznawialna, z postępem.
- Optymalizacja na maksa [decyzja autora]: kompresja zstd każdego obiektu przed szyfrowaniem (z pominięciem nieściśliwych), równoległy transfer, stałe połączenia (keep-alive + WebSocket, bez pollingu), deduplikacja chunków, build release z LTO.

## 17. Bezpieczeństwo danych — wymagania twarde

- Każdy zapis pliku na dysk: atomowy (temp + rename), po wcześniejszym zapisaniu stanu w snapshocie — wszystko odwracalne.
- Daemon rozpoznaje własne zapisy (brak pętli zwrotnej watchera).
- Zapisy do `.git` tylko gdy git bezczynny; po zapisie stan repo musi przechodzić `git fsck` / być spójny dla `git status`.
- Przy niepewności — wstrzymaj i zapytaj, nie zgaduj.
- Ochrona przed masowym usunięciem [moja decyzja]: jeśli katalog projektu zniknął (np. odmontowany dysk) albo jeden snapshot usuwa > 30% śledzonych plików i > 50 plików, projekt jest wstrzymywany z powiadomieniem; `spoko resume --confirm` wypycha usunięcie.
- Plik zmieniony lokalnie może zostać nadpisany przez zmianę z drugiego komputera tylko w dwóch przypadkach: fast-forward (brak lokalnych zmian) albo czysty 3-way merge (sekcja 7). Konfliktowa wersja nigdy nie nadpisuje pliku na aktywnym komputerze.

## 18. Testowanie (wymagania dla planu)

- Symulacja wielu urządzeń w jednym procesie / na jednej maszynie (wiele daemonów, wiele katalogów, jeden serwer).
- CI na macOS, Linux, Windows.
- Testy scenariuszy: rozjazd offline, konflikty tekstowe, konflikty binarne, zmiana gałęzi z niezacommitowanymi zmianami, rebase w toku, kolizje wielkości liter, nazwy z Windows, CRLF, Claude Code z różnymi ścieżkami, volumeny.
- Testy właściwościowe/fuzz dla merga i replikacji gita: po synchronizacji `git status`, `git log`, `git stash list` identyczne na obu stronach.
- Empiryczna weryfikacja przenośności volumenów baz między ARM64 i x86-64 (sekcja 12).

## 19. Pozostawione builderowi

Wybór bibliotek (git, watcher, kryptografia, tray, HTTP/WS), format protokołu i przechowywania, parametry chunkingu i debounce, dokładne nazwy komend i kluczy configu, szczegóły retencji, lista kluczy `.git/config` do pominięcia, reguły kodowania ścieżek Claude Code na Linux/Windows, obsługa symlinków na Windows, szczegóły UI trayu — w granicach wymagań z tego dokumentu.

## 20. Decyzje projektanta do sprawdzenia przez autora

1. Replikacja stanu gita semantycznie (czytanie refów/indexu/stashy biblioteką git), nie kopiowanie katalogu `.git` — zastępuje wcześniejszy pomysł z ukrytymi refami.
2. Serwer to zaszyfrowany magazyn blobów + wskaźnik head z CAS, nie repo git — zastępuje wcześniejszy pomysł z natywnym protokołem git (niezgodny z E2E). Merge wyłącznie po stronie klienta.
3. Tray w MVP (minimalny) + powiadomienia systemowe.
4. Rozjazd tej samej gałęzi na dwóch komputerach → druga wersja jako gałąź `<nazwa>-<urządzenie>`.
5. Operacje gita w toku (rebase/merge) nie są replikowane w MVP; `gitstate` czeka na ich koniec.
6. Worktrees i submoduły w MVP tylko z ostrzeżeniem.
7. Domyślny tryb hooków `ask`.
8. Docker tylko przez Docker API; volumeny po nazwie logicznej.
9. Próg „binarny” 10 MB; domyślne ignory `.DS_Store`, `Thumbs.db`, `.idea/workspace.xml`.
10. Preset Claude Code obowiązkowy w MVP, Cursor/Codex jeśli starczy czasu.
11. MVP serwera: self-host, konta zakładane komendą admina, tokeny urządzeń, SQLite + dysk/S3; publiczna usługa wielokontowa w etapie 2.
12. Cele wydajności z sekcji 16.
13. Dołączenie komputera z istniejącą kopią: stan z serwera wygrywa, lokalne różnice w snapshocie „sprzed dołączenia” z `spoko restore --from-join`.
14. Pełne lustro historii gita przez spoko (bez pobierania z origin) w MVP.
15. Otwarty konflikt: jeden head, wersja aktywnego komputera na obu dyskach, druga w `meta/conflicts`; czysty merge zapisywany po cichu.
