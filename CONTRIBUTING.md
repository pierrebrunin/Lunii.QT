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
│   ├── src/convert/         images (BMP RLE4, LIFF) et audio (MP3, ffmpeg, volume)
│   ├── src/device/          trait Device, Lunii v1/v2/v3, Flam ; common.rs : import
│   │                        STUdio et ajout de pistes partagés (trait StoryWriter)
│   ├── src/album/           dossier de MP3 → histoire : scan.rs (tags, sous-dossiers,
│   │                        pochette), layout.rs (story.json), screens.rs (écrans)
│   ├── src/podcast.rs       flux RSS, téléchargement, préparation de l'import
│   ├── src/settings.rs      réglages (~/.lunii/settings.json)
│   ├── src/firmware.rs      téléchargement des firmwares
│   ├── src/tools.rs         import base STUdio, ffmpeg, mise à jour
│   └── tests/               scénarios complets sur de faux appareils
├── lunii-cli/         outil en ligne de commande
└── lunii-gui/         interface graphique Slint (binaire `lunii`)
    ├── ui/                  écrans .slint (fenêtre principale, liste, dialogues, journal)
    ├── src/app/             contrôleur (port de main_window.py / nm_window.py / debug_ui.py) :
    │                        mod.rs, actions.rs (menus, enum Action), podcast.rs, log_panel.rs
    ├── src/worker.rs        tâches longues en arrière-plan (port de ierWorker.py)
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
- **ffmpeg** (facultatif) : transcodage de l'audio non conforme, podcasts M4A,
  égalisation du volume. Il est cherché dans `~/.lunii`, puis dans le
  `PATH`. Sans lui, les tests qui en ont besoin sont ignorés (ils l'indiquent).

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
- **ffmpeg** n'est pas inclus : sous Windows et macOS, le menu Outils le
  télécharge. Sous Linux, c'est celui du système.

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
packaging/linux/package.sh v3.1.4 dist                      # Linux
pwsh packaging/windows/package.ps1 -Version v3.1.4          # Windows (Inno Setup 6)
packaging/macos/package.sh v3.1.4 dist                      # macOS, après les builds
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
histoires (sélection multiple Ctrl/Maj, flèches, Ctrl+A, menu contextuel),
vignette et description, barres de progression avec bouton d'arrêt, barre
d'état (un clic sur le SNU copie les détails de l'appareil), menus et raccourcis
(Ctrl+I, Ctrl+S, Suppr, Alt+↑/↓, Ctrl+↑/↓, Ctrl+H, Ctrl+N, Ctrl+L, F5, F1…),
réglages du mode nuit, connexion Luniistore pour le firmware, journal de debug,
vérification des mises à jour, glisser-déposer d'archives.

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
  la musique en stéréo, sont convertis par ffmpeg, qui est alors nécessaire.
- **Sous-dossiers** : un dossier sans MP3 mais avec des sous-dossiers de MP3
  (CD, chapitres, saisons…) donne une histoire à deux menus : on choisit
  d'abord le sous-dossier (écran avec son nom), puis la piste. HOME remonte d'un
  menu. À la fin du dernier titre d'un sous-dossier, le suivant démarre. Les
  sous-dossiers sont triés dans l'ordre naturel (`CD 2` avant `CD 10`). On ne
  peut pas ajouter de pistes à ces albums.
- **Volume** : Outils / **Égaliser le volume** (ffmpeg, réglage retenu) ramène
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

Histoires / **Importer un podcast** (Ctrl+P) : coller l'adresse de la page du
podcast (par exemple `https://www.radiofrance.fr/franceinter/podcasts/les-odyssees`)
ou directement son flux RSS, puis **choisir les épisodes** à importer (filtre,
Tous / Aucun).

- Deux façons de les importer, au choix dans la fenêtre :
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
- **Mettre à jour les podcasts** (menu Histoires) : pour chaque podcast importé
  en album et présent sur l'appareil, relit son flux et ajoute les épisodes pas
  encore importés, sans rien demander. L'opération peut être interrompue.
- Les épisodes de Radio France sont en M4A : ffmpeg est nécessaire pour les
  convertir. Il lit depuis un fichier temporaire et non depuis un pipe, car
  l'index de ces fichiers est à la fin.
- Les podcasts de Radio France sont destinés à l'écoute privée : ne pas
  redistribuer les fichiers.

**Ajouter des MP3 à un album** (Lunii et Flam) : sélectionner l'album, puis Histoires /
Ajouter des MP3 à l'album. Un album créé par Lunii se reconnaît à ses noms de
fichiers sur l'appareil (`sf/000/SNDCOVER`, `TRK00001`…), sans fichier
supplémentaire.
- Seuls les fichiers des nouvelles pistes sont écrits (écran, audio),
  à la suite des pistes existantes. Les tables `ni`/`ri`/`si`/`li` (et sur
  Lunii v1/v2 le fichier d'autorisation `bt`, dérivé de `ri`) sont
  régénérées en dernier : si l'opération est interrompue, l'album reste tel
  qu'il était.
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
| `LUNII_SHOT_FEED=<flux.xml>` | (captures) ajoute la fenêtre podcast, avec ce flux RSS local |
| `LUNII_SHOT_IMPORT=<a>:<b>` | (captures) importe ces archives par l'interface, pour capturer la progression |

Captures d'écran sans affichage (rendu logiciel), avec la feature `screenshot` :

```bash
cargo build -p lunii-gui --features screenshot
LUNII_DEVICES=/chemin/faux_lunii LUNII_NO_UPDATE_CHECK=1 \
LUNII_SCREENSHOT=/tmp/captures target/debug/lunii
```

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
suppression des tags, transcodage ffmpeg) et Flam plain.

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
- **MP3 illisible** dans une archive STUdio : transcodé par ffmpeg au lieu de
  faire échouer l'import.
- **Mode nuit sur une histoire masquée** : agit sur le bon dossier.
- **Export interrompu ou en erreur** : l'archive partielle est supprimée.

Le redimensionnement des images (bicubique) n'est pas identique au bit près à
celui de la version 3.1 (écart moyen mesuré inférieur à 0,1/255).

Les messages de la bibliothèque sont en anglais ; l'interface les traduit à
l'affichage.
