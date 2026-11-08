# Lunii — développement

Lunii est écrit en Rust, avec une interface [Slint](https://slint.dev).
Les versions 3.1 et antérieures, en Python, restent consultables dans
l'historique git, jusqu'au commit `c8afe43`.

## Organisation

```
.
├── lunii-core/        bibliothèque, sans dépendance à une interface
│   ├── src/constants.rs     constantes, répertoire ~/.lunii
│   ├── src/crypto.rs        XXTEA (rounds Lunii), AES-CBC
│   ├── src/db.rs            bases officielle / tierce, cache des vignettes
│   ├── src/story.rs         Story, StoryList, STUdio, détection des archives
│   ├── src/archive.rs       lecture uniforme zip / 7z
│   ├── src/convert/         images (BMP RLE4, LIFF) et audio (MP3, conversion, volume)
│   ├── src/device/          trait Device, Lunii v1/v2/v3, Flam ; common.rs : import
│   │                        STUdio et modification d'album partagés (trait StoryWriter)
│   ├── src/album/           dossier de MP3 → histoire : scan.rs (tags, sous-dossiers,
│   │                        pochette), layout.rs (story.json), screens.rs (écrans),
│   │                        meta.rs (titres et durées des pistes, album.json)
│   ├── src/podcast/         podcasts : mod.rs (types, chargement d'une adresse),
│   │                        feed.rs (RSS, Atom), page.rs (pages sans flux,
│   │                        schema.org), html.rs, apple.rs (liens, recherche),
│   │                        import.rs (téléchargement, préparation de l'import),
│   │                        state.rs (podcasts suivis)
│   ├── src/settings.rs      réglages (~/.lunii/settings.json)
│   ├── src/firmware.rs      téléchargement des firmwares
│   ├── src/tools.rs         import base STUdio, téléchargements, mise à jour
│   └── tests/               scénarios complets sur de faux appareils
├── lunii-cli/         outil en ligne de commande
└── lunii-gui/         interface graphique Slint (binaire `lunii`)
    ├── ui/                  écrans .slint (fenêtre, liste, vue Podcasts, pistes d'album, dialogues, journal)
    ├── src/app/             contrôleur de la fenêtre (mode nuit, journal) :
    │                        mod.rs (état, événements),
    │                        devices.rs (recherche et ouverture des appareils),
    │                        stories.rs (liste, sélection, détails, durées),
    │                        progress.rs (tâches, temps restant), actions.rs (menus,
    │                        enum Action), podcast.rs (vue Podcasts), album.rs
    │                        (pistes d'un album), log_panel.rs (journal)
    ├── src/worker.rs        tâches longues en arrière-plan
    ├── src/i18n.rs          traductions des messages côté Rust (un test vérifie que les
    │                        messages de lunii-core sont tous traduits)
    ├── src/logfile.rs       fichier de log persistant et logger de la façade `log`
    └── lang/fr/…/lunii-gui.po  traduction française
```

Les appareils rendent compte des opérations longues par le trait `Reporter`
(logs + progression), et s'interrompent par `Context::request_abort()` :
l'interface lance ces opérations dans un thread (`worker.rs`).

Le répertoire de configuration est `~/.lunii` : bases, cache, clés v3 et
sauvegardes `.md` / `.mdf`. Celui des versions précédentes, `~/.lunii-qt`, est
renommé en `~/.lunii` au premier lancement.

## Prérequis

- **[mise](https://mise.jdx.dev)** : les versions des outils (Rust avec
  `rustfmt` et `clippy`) sont fixées dans `mise.toml`, à la racine du
  dépôt, et nulle part ailleurs. La CI lit le même fichier. Pour les
  installer :
  ```bash
  mise install
  ```
  mise installe rustup s'il manque, puis la toolchain demandée. Pour changer
  de version de Rust, il suffit de modifier `mise.toml`.
- **Linux** : un compilateur C, `pkg-config` et les en-têtes fontconfig
  (Slint). Sur Debian / Ubuntu :
  ```bash
  sudo apt install build-essential pkg-config libfontconfig1-dev
  ```
  À l'exécution, les bibliothèques X11 / Wayland et xkbcommon d'un bureau
  standard suffisent. Les boîtes de dialogue de fichiers passent par
  `xdg-desktop-portal`.
- **Windows** : la chaîne MSVC (Build Tools de Visual Studio, « Développement
  Desktop en C++ »).
- **macOS** : les outils en ligne de commande Xcode (`xcode-select --install`).

## Compilation

```bash
cd rust
cargo build --release          # target/release/lunii (interface) et lunii-cli
cargo run -p lunii-gui         # lance l'interface (debug)
cargo run -p lunii-cli -- --help
```

## Développement

Avant de pousser, les mêmes vérifications que la CI (`.github/workflows/ci.yml`) :

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p lunii-gui --features screenshot --locked
cargo test --workspace --locked
```

`--locked` échoue si `Cargo.lock` n'est pas à jour : après un changement de
dépendance ou de version, lancer `cargo build`, puis commiter `Cargo.lock`.

- **Tests** : `lunii-core/tests/` joue les parcours complets (import STUdio,
  albums, ajout de pistes, podcasts) sur de faux appareils Lunii v2 et Flam
  créés dans un dossier temporaire. Aucun appareil réel ni réseau n'est
  nécessaire. Le répertoire de configuration est redirigé par `set_cfg_dir`.
- **Sans appareil branché** : `LUNII_DEVICES` pointe l'interface vers un
  dossier (par exemple une copie du contenu d'une Lunii ou d'une Flam, avec
  son `.md` / `.mdf`) au lieu de détecter les appareils, voir
  [les variables](#variables-de-développement).
- **Nouveau texte affiché** :
  - texte Slint : `@tr("...")` ;
  - texte Rust : `tr()` / `trf()` ;
  - message de `lunii-core` : en anglais, traduit à l'affichage.

  Dans tous les cas, ajouter sa traduction au `.po` (voir
  [Traductions](#traductions)). Le test
  `core_messages_are_translated` échoue si un message de `lunii-core` n'a pas
  de traduction.

## CI et releases

Les workflows sont dans `.github/workflows/` (à la racine du dépôt) :

- **`ci.yml`** : à chaque push sur `main` et pour chaque pull request, format
  (`cargo fmt --check`), clippy (`-D warnings`) et tests sur Linux, Windows et
  macOS.
- **`release.yml`** : sur un tag `vX.Y.Z`, construit les paquets et les publie
  dans une **release GitHub en brouillon**, à relire puis publier à la main. Un
  lancement manuel (« Run workflow ») ne fait que construire les paquets,
  disponibles comme artefacts.

| Système | Paquets |
|---------|---------|
| Linux x86_64 | `Lunii-vX.Y.Z-x86_64.AppImage`, `Lunii-vX.Y.Z-Linux-x86_64.tar.gz` |
| Windows x64 | `Lunii-vX.Y.Z-win64_setup.exe` (installation sans droits administrateur), `Lunii-vX.Y.Z-win64_portable.zip` |
| macOS 11+ | `Lunii-vX.Y.Z-MacOs-universal.dmg` (Apple Silicon et Intel) |

- **Linux** : compilé sur Ubuntu 22.04, donc compatible avec glibc 2.35 et
  plus récent. `Lunii-….AppImage cli <args>` lance l'outil en ligne de
  commande.
- **macOS** : l'application n'a qu'une signature ad hoc, sans certificat
  développeur Apple ni notarisation. Au premier lancement, il faut l'autoriser
  (voir la section macOS du README principal).

La logique de packaging est dans `packaging/` (un script par système), et
fonctionne aussi en local. Outils nécessaires :
- Linux : `desktop-file-utils` ; `appimagetool` est pris dans `$APPIMAGETOOL`
  ou le `PATH`, sinon téléchargé.
- Windows : PowerShell et Inno Setup 6.
- macOS : `lipo` et `codesign` (Xcode), `create-dmg` (facultatif, sinon
  `hdiutil`), plus les cibles
  `rustup target add aarch64-apple-darwin x86_64-apple-darwin`.

```bash
cargo build --release
packaging/linux/package.sh v4.0.0 dist                      # Linux
pwsh packaging/windows/package.ps1 -Version v4.0.0          # Windows (Inno Setup 6)
packaging/macos/package.sh v4.0.0 dist                      # macOS, après les builds
                                                            # --target aarch64/x86_64-apple-darwin
```

**Faire une release :**

1. Changer `version` dans `Cargo.toml` (`[workspace.package]`), puis
   lancer `cargo build` pour mettre à jour `Cargo.lock`.
2. Commiter, puis créer et pousser le tag :
   `git tag v3.2.0 && git push origin v3.2.0`. La release vérifie que le tag
   correspond à `Cargo.toml`.
3. Relire la release en brouillon sur GitHub (paquets, notes), puis la publier.
   L'application signale la nouvelle version à ses utilisateurs.

## Interface graphique

```bash
cargo run --release -p lunii-gui
```

Elle propose : sélection de l'appareil, filtre, liste des
histoires (sélection multiple Ctrl/Maj, flèches, Ctrl+A, menu contextuel ;
colonnes durée, taille, base O/T et UUID au choix dans Outils, enregistrées dans
`~/.lunii/settings.json`, base et UUID masqués par défaut ; la durée vient de la
base officielle, de `album.json` pour les albums et podcasts importés par
l'application, ou est estimée en arrière-plan d'après les trames MP3 des
fichiers `sf/`, voir `lunii-core/src/mp3.rs`),
vignette et description, barres de progression avec bouton d'arrêt, barre
d'état (un clic sur le SNU ouvre les informations de l'appareil : modèle, firmware,
emplacement, stockage, clés ; aussi par Fichier / Informations sur l'appareil), menus et raccourcis
(Ctrl+I, Ctrl+S, Suppr, Alt+↑/↓, Ctrl+↑/↓, Ctrl+H, Ctrl+N, Ctrl+L, F5, F1…),
réglages du mode nuit, connexion Luniistore pour le firmware, journal de debug,
vérification des mises à jour, glisser-déposer d'archives.

Deux vues, au choix en haut de la fenêtre : **Histoires** (la liste de
l'appareil) et **Podcasts**. Les dialogues (suppression, connexion, mode nuit, à
propos) sont modaux : tant que l'un est ouvert, les menus et leurs raccourcis
sont désactivés ; Échap ou la croix le ferme.

### Albums MP3

Histoires / **Importer un album MP3** (Ctrl+Maj+I) transforme un dossier de MP3
(musique, livre audio…) en histoire. On peut aussi glisser-déposer le dossier.

- **Navigation** : depuis la couverture, OK ouvre un menu avec une entrée par
  piste, qu'on parcourt à la molette (l'écran affiche le titre). OK lance la
  piste, et les suivantes s'enchaînent. À la fin, ou avec HOME, on revient au
  menu. OK pendant la lecture passe à la piste suivante.
- **Titres et ordre** : ils viennent des tags ID3 (titre, album, artiste, disque,
  numéro de piste), sinon du nom du fichier et du dossier. La couverture est la
  pochette ID3, sinon l'image du dossier (`cover`, `folder`, `front` ou `album`
  en jpg/png/webp…, ou sa seule image), sinon elle est générée avec le titre de
  l'album. Les écrans
  (320×240, niveaux de gris) sont dessinés avec la police DejaVu Sans Bold
  embarquée (`res/fonts`).
- **Audio** : les appareils veulent du MP3 mono 44,1 kHz. Les autres MP3, comme
  la musique en stéréo, sont convertis (voir [Conversion audio](#conversion-audio)).
- **Sous-dossiers** : un dossier sans MP3 mais avec des sous-dossiers de MP3
  (CD, chapitres, saisons…) donne une histoire à deux menus : on choisit
  d'abord le sous-dossier (écran avec son nom), puis la piste. HOME remonte d'un
  menu. À la fin du dernier titre d'un sous-dossier, le suivant démarre. Les
  sous-dossiers sont triés dans l'ordre naturel (`CD 2` avant `CD 10`). On ne
  peut pas ajouter de pistes à ces albums.
- **Volume** : Outils / **Égaliser le volume** (réglage retenu) ramène
  toutes les pistes des albums et des podcasts au même niveau sonore (EBU R128,
  -16 LUFS), utile pour mélanger des sources différentes. Chaque piste est alors
  réencodée, l'import est un peu plus long.
- **Identifiant** : l'UUID de l'histoire est dérivé de l'album (v5), donc le même
  album importé deux fois est refusé.
- **Vérification** : après l'écriture d'une histoire STUdio (album compris) ou
  l'ajout de pistes, Lunii vérifie que chaque écran et chaque son référencé
  est présent et non vide, et que les tables d'index se relisent telles
  qu'écrites. Si une histoire importée échoue à cette vérification (appareil
  débranché, plein…), elle est supprimée pour pouvoir être importée à nouveau.

Le dossier est converti en archive STUdio temporaire (`album/`), puis importé
par le chemin STUdio habituel.

### Podcasts

La vue **Podcasts** (ou Histoires / Importer un podcast, Ctrl+P, qui ouvre
directement la fenêtre d'ajout) :
- **à gauche, les podcasts suivis**. **+** ouvre la fenêtre « Ajouter un
  podcast », avec un seul champ :
  - des **mots** : recherche dans le catalogue Apple Podcasts (API publique
    `itunes.apple.com/search`, sans clé, environ 20 requêtes par minute, d'où
    une recherche sur Entrée seulement ; catalogue du pays de la langue du
    système). Les podcasts dont l'éditeur ne donne pas le flux à Apple sont
    grisés (« flux non public ») : on colle alors l'adresse de leur page ;
  - une **adresse** `http(s)://` : son flux RSS ou Atom (le format standard des
    podcasts), sa page web si elle annonce son flux (`<link rel="alternate"
    type="application/rss+xml">`), ou son lien Apple Podcasts (le flux est
    demandé à l'API iTunes).

  Le podcast choisi est chargé et vérifié (titre, nombre d'épisodes) avant que
  **Suivre** ne soit possible. Une page **sans flux** est lue avec les
  standards du web : la liste de ses épisodes (schema.org `ItemList`, pages
  suivantes par `<link rel="next">`), puis la page de chaque épisode (schema.org
  `RadioEpisode`, `PodcastEpisode`… avec son `AudioObject`) ; les pages listées
  sans audio sont ignorées. Titre, éditeur et couverture viennent du
  schema.org et des balises Open Graph (`og:title`, `og:site_name`,
  `og:image`). Aucun site n'est traité à part : c'est ainsi que fonctionnent,
  par exemple, les séries et sélections de radiofrance.fr.
  Les flux des podcasts suivis sont chargés en arrière-plan,
  sans bloquer l'application. Sous chaque podcast : ses nouveaux épisodes si
  son album est sur l'appareil, son nombre d'épisodes sinon. Ils sont
  enregistrés dans `~/.lunii/podcasts.json`, comme les épisodes importés ;
- **à droite, les épisodes** du podcast sélectionné, à cocher (filtre, Tous /
  Aucun), le mode d'import, puis **Importer (n)**. ↻ recharge le flux, **Ne plus
  suivre** retire le podcast de la liste (son album reste sur l'appareil, mais
  n'est plus mis à jour).

- Deux façons de les importer :
  - **un album** (par défaut, sur Lunii comme sur Flam) : une seule histoire,
    un épisode par piste, dans l'ordre chronologique, avec un menu pour choisir
    l'épisode et l'image du podcast en couverture ;
  - **une histoire par épisode** : chaque épisode apparaît dans la liste de
    l'appareil comme une histoire, avec son titre à l'écran, et se lance
    directement.
- En mode « une histoire par épisode », l'UUID de chaque histoire est dérivé du
  flux et de l'épisode : les épisodes déjà présents sur l'appareil sont reconnus
  et grisés.
- L'UUID de l'album est dérivé de l'adresse du flux. Si l'album est déjà sur
  l'appareil, les épisodes choisis y sont **ajoutés**. Les épisodes déjà importés
  sont retenus dans `~/.lunii/podcasts.json` et grisés dans la liste.
- Un même podcast a souvent plusieurs adresses de flux (celle d'une page,
  celle d'Apple Podcasts, une ancienne adresse redirigée). Ajouté une deuxième
  fois sous une autre adresse, il est reconnu comme déjà suivi, et garde
  l'adresse sous laquelle il l'était, donc le même album. Il est reconnu par
  les adresses que déclarent ses flux (`<itunes:new-feed-url>`,
  `<atom:link rel="self">`, retenues dans `podcasts.json`). Pour un podcast
  suivi avant cet enregistrement, le même titre et un épisode déjà importé
  suffisent.
- Les épisodes qui ne sont pas en MP3 (souvent en M4A/AAC) sont convertis
  (voir [Conversion audio](#conversion-audio)).
- Les podcasts sont en général destinés à l'écoute privée : ne pas
  redistribuer les fichiers.

**Pistes d'un album** (Lunii et Flam, albums MP3 et podcasts importés en
album) : le panneau de détails liste les pistes de l'album sélectionné, par
sous-dossier s'il y en a, et permet de les modifier. Un album créé par Lunii se reconnaît à ses noms de
fichiers sur l'appareil (`sf/000/SNDCOVER`, `TRK00001`…). Les titres, qui ne
sont sinon que dessinés dans les écrans, sont gardés en texte dans
`album.json`, dans le dossier de l'histoire (JSON non chiffré, ignoré par les
conteuses et exclu des exports « plain »), avec la durée de chaque piste,
mesurée sur l'audio au moment où il est écrit (colonne Durée exacte, sans
relire l'audio). Pour les albums importés avant ce fichier, les titres sont
inconnus : « Piste 1 », « Piste 2 »… ; avant les durées, la durée est estimée.

**Modifier un album** : dans le panneau de détails de l'album, choisir une
piste puis ▲ / ▼ / Supprimer, ou Ajouter des MP3… ; Appliquer écrit tout en une
fois, Annuler revient à l'album de l'appareil. Changer de sélection abandonne
les changements non appliqués. Histoires / Modifier l'album affiche le panneau
s'il est masqué ; Ajouter des MP3 à l'album ajoute directement à la fin. Pas
disponible pour les albums à sous-dossiers (leurs pistes sont seulement
listées).
- Les nouvelles pistes et les écrans des pistes déplacées (leur numéro change)
  sont d'abord préparés dans un dossier temporaire : si l'opération est
  interrompue à ce stade, l'album reste tel qu'il était. Les fichiers sont
  ensuite renommés sur place, puis les tables `ni`/`ri`/`si`/`li` (et sur
  Lunii v1/v2 le fichier d'autorisation `bt`, dérivé de `ri`) régénérées.
- Une piste déplacée dont le titre est inconnu garde son ancien écran, donc
  son ancien numéro.
- La description « Artiste - N MP3 » suit le nouveau nombre de pistes.
- Les écrans de piste affichent leur numéro seul, sans « n / total », qui
  deviendrait faux après un ajout. Les albums importés avant ce changement
  gardent leurs « n / total » sur les pistes déjà présentes.

La langue suit le système (français ou anglais) ; `LUNII_LANG=en` ou
`LUNII_LANG=fr` la force.

#### Variables de développement

| Variable | Effet |
|----------|-------|
| `LUNII_DEVICES=/chemin/a:/chemin/b` | remplace la détection des appareils (faux appareils de test) |
| `LUNII_NO_UPDATE_CHECK=1` | pas de requête vers GitHub au démarrage |
| `LUNII_WAYLAND=1` | garde le backend Wayland natif au lieu de X11 (XWayland) |
| `LUNII_CFG_DIR=…` | autre répertoire de configuration que `~/.lunii` |
| `LUNII_LANG=fr` / `en` | force la langue |
| `LUNII_SCREENSHOT=<dossier>` | (feature `screenshot`) rend les écrans en PNG sans affichage, puis quitte |
| `LUNII_SHOT_FEED=<flux.xml>` | (captures) ajoute la vue Podcasts, avec ce flux RSS local suivi |
| `LUNII_SHOT_SEARCH=<mots ou adresse>` | (captures, avec `LUNII_SHOT_FEED`) ajoute la fenêtre d'ajout d'un podcast : vraie recherche (réseau), premier résultat vérifié puis suivi |
| `LUNII_SHOT_IMPORT=<a>:<b>` | (captures) importe ces archives ou dossiers par l'interface ; si le dernier est un album, capture aussi ses pistes et son dialogue de modification (qui est appliquée) |

Captures d'écran sans affichage (rendu logiciel), avec la feature `screenshot` :

```bash
cargo build -p lunii-gui --features screenshot
LUNII_DEVICES=/chemin/faux_lunii LUNII_NO_UPDATE_CHECK=1 \
LUNII_SCREENSHOT=/tmp/captures target/debug/lunii
```

### Conversion audio

Les appareils veulent du MP3 mono 44,1 kHz sans tags. Un fichier audio est
converti s'il n'a pas l'extension `.mp3`, ou si c'est un MP3 stéréo, à moins de
44,1 kHz, en débit moyen (ABR) ou illisible. Sinon, seuls ses tags sont
retirés.

La conversion est faite dans l'application, en Rust, sans programme externe
(`lunii-core/src/convert/transcode.rs`) :
- **décodage** avec Symphonia : MP3, AAC/M4A, Vorbis, FLAC, WAV, ALAC, AIFF…
  Le format est reconnu d'après le contenu, pas l'extension. L'Opus n'est pas
  pris en charge ;
- **mono** : les canaux sont additionnés à -3 dB chacun, comme le faisait
  ffmpeg (même niveau pour une voix enregistrée en stéréo), puis baissés si
  la somme dépasse la pleine échelle ;
- **rééchantillonnage** à 44,1 kHz avec Rubato (FFT) ;
- **volume** (option) : sonie mesurée selon EBU R128 (crate `ebur128`), puis
  un gain unique pour viser -16 LUFS, limité pour garder les crêtes sous
  -1,5 dBTP. Contrairement au filtre `loudnorm` de ffmpeg, la dynamique de la
  piste n'est pas modifiée ;
- **encodage** avec rusty_mp3 en débit constant de 64 kbit/s, sans la trame
  d'en-tête Info/Xing.

### Traductions

Les textes de l'interface Slint (`@tr(...)`) et les messages produits en Rust
utilisent le même fichier `lunii-gui/lang/fr/LC_MESSAGES/lunii-gui.po`, à
modifier directement (une entrée `msgid` / `msgstr` par texte). Il est inclus
dans le binaire à la compilation.

Les messages de `lunii-core` (en anglais) sont traduits à l'affichage en les
comparant aux modèles `{}` du catalogue (« Missing {} in {} »). Les arguments
nommés du code (`{name}`) s'écrivent `{}` dans le `.po`.

### Différences avec les versions Python (3.1 et antérieures)

- **Glisser-déposer** : il fonctionne sous Windows, macOS et X11. winit 0.30 ne
  le gère pas sous Wayland natif. Dans une session Wayland où XWayland est
  disponible (`DISPLAY` défini), l'appli utilise donc X11 automatiquement
  (`LUNII_WAYLAND=1` force le Wayland natif, sans glisser-déposer).
- **Journal** : au lieu d'une fenêtre séparée, c'est un panneau repliable en bas
  de la fenêtre principale (Ctrl+L, ou l'icône à droite de la barre d'état).
  On le redimensionne avec la poignée au-dessus. L'icône affiche un point orange
  ou rouge si un avertissement ou une erreur est arrivé pendant que le panneau
  était fermé. Le bouton « Copier » met les lignes affichées (filtrées) dans le
  presse-papiers.
  Chaque session est aussi écrite dans `~/.lunii/logs/lunii.log`, avec un
  horodatage et sans les lignes de progression. Les quatre sessions précédentes
  sont gardées (`lunii.log.1` à `.4`), et le bouton « Ouvrir le dossier des
  journaux » ouvre ce dossier.
- **Détails des histoires** : ils s'affichent dans un panneau à droite de la
  liste, présent en permanence (sauf si la fenêtre fait moins de 760 px de
  large, ou si Outils / Détails des histoires est décoché). La liste garde
  donc sa taille quand on sélectionne une histoire. Sans sélection, ou avec
  plusieurs histoires sélectionnées, le panneau l'indique. Sa largeur se règle
  avec la poignée à sa gauche (de 240 px jusqu'à laisser 420 px à la liste) et
  est enregistrée dans `settings.json` (`details_width`).
- **Démarrage** : la base officielle est chargée en arrière-plan. La fenêtre
  s'ouvre tout de suite et les noms s'affichent dès que la base est prête.
- **Rafraîchissement de la base** et **téléchargement du firmware** : ils
  tournent en arrière-plan au lieu de bloquer l'interface.
- **Raccourci F2** (déblocage de l'export des histoires officielles) : il n'est
  pas repris.
- **Factory reset** : l'entrée était cachée et non implémentée ; elle n'est
  pas reprise.
- **Mode nuit d'une histoire masquée** : il est conservé à la réouverture de
  l'appareil. La version 3.1 le cherchait dans `.content/` au lieu de
  `.content.hidden/`.

## Utilisation du CLI

```bash
lunii-cli devices                      # appareils détectés
lunii-cli info                         # firmware, SNU, clés
lunii-cli list --sizes
lunii-cli import histoire.plain.pk dossier_d_archives/
lunii-cli export 1A2B3C4D -o ~/sauvegardes
lunii-cli export --all -o ~/sauvegardes
lunii-cli move 1A2B3C4D --top
lunii-cli hide 1A2B3C4D
lunii-cli night-mode 1A2B3C4D          # Lunii uniquement
lunii-cli nm-config --enable true --stories 3
lunii-cli remove 1A2B3C4D
lunii-cli recover --dry-run
lunii-cli db import-studio unofficial.json
lunii-cli firmware --email moi@exemple.fr -o .
```

`-d/--device <point de montage>` choisit l'appareil (par défaut le premier
trouvé), `-v` affiche les messages de debug, `Ctrl-C` interrompt proprement
l'opération en cours.

Comme dans les versions précédentes, l'ouverture d'un appareil réécrit son index
(`.pi` / `etc/library/list`).

## Différences avec la version 3.1

Le comportement a été vérifié octet par octet contre la version 3.1 :
mêmes fichiers sur l'appareil et mêmes exports pour Lunii v2, Lunii v3 et Flam,
avec des archives plain.pk, v2 (zip et 7z), STUdio (conversion d'images,
suppression des tags, transcodage, alors par ffmpeg) et Flam plain.

Écarts volontaires, qui corrigent des bugs de la version 3.1 :

- **Restauration d'une sauvegarde Lunii v3** : la version 3.1 écrivait l'histoire dans
  `.content/<UUID complet>/` au lieu de `.content/<8 derniers caractères>/`.
- **Images STUdio au format BMP** : la version 3.1 les convertissait à
  l'envers (elle ne retournait que les sources non BMP, alors que les BMP sont déjà
  décodées dans le bon sens).
- **Export des histoires officielles** : refusé dans la bibliothèque elle-même
  pour les Lunii v1/v2. La version 3.1 ne le bloquait que dans l'interface, et seulement
  quand une seule histoire était sélectionnée. Sur Lunii v3 et sur Flam, une
  histoire officielle n'est exportée que sous forme de sauvegarde chiffrée, à
  remettre sur un appareil, jamais déchiffrée. La version 3.1 l'exportait en
  clair dès que les clés étaient connues.
- **Récupération des histoires perdues** : la version 3.1 ne trouvait rien si
  `.content.hidden/` n'existait pas.
- **Vignette Flam manquante** : avertissement au lieu d'un plantage.
- **Archive STUdio en 7z** : les tags ID3 sont supprimés, comme pour le zip.
- **MP3 illisible** dans une archive STUdio : transcodé au lieu de
  faire échouer l'import.
- **Mode nuit sur une histoire masquée** : agit sur le bon dossier.
- **Export interrompu ou en erreur** : l'archive partielle est supprimée.

Le redimensionnement des images (bicubique) n'est pas identique au bit près à
celui de la version 3.1 (écart moyen mesuré inférieur à 0,1/255).

Les messages de la bibliothèque sont en anglais ; l'interface les traduit à
l'affichage.
