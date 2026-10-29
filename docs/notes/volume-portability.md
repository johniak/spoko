# Przenośność volumenów Dockera ARM64 ↔ x86-64 (spec §12)

**Pytanie:** czy katalog danych Postgresa utworzony na `linux/arm64` (Mac M-series) uruchomi się
na `linux/amd64` (Linux/Windows x86) z tym samym obrazem i tą samą wersją major?

**Jak sprawdzane:** `scripts/volume-portability.sh` (job CI `volume-portability`, qemu): baza
z tabelą i indeksem tekstowym na arm64 → stop → ten sam volumen pod amd64 → zapytanie po indeksie.

**Wynik:** _uzupełniany z logu CI_ — patrz niżej.

**Decyzja domyślna (niezależnie od wyniku):** `on_mismatch = "warn"` — przy różnej architekturze,
obrazie albo wersji major `spoko volume pull` niczego nie zmienia, pokazuje różnice i pyta;
`--force` przywraca mimo to, a `on_mismatch = "dump"` (z `dump`/`restore`) przełącza na
logiczny dump, który jest przenośny zawsze. Ryzyko kolacji (debian vs alpine / różne glibc)
nie jest wykrywalne testem startu, więc ostrzeżenie zostaje nawet przy udanym starcie.
