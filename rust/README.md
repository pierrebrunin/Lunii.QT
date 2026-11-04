# Lunii.QT — portage Rust

Version Rust de Lunii.QT. Elle remplace l'application Python, qui a été
supprimée du dépôt ; elle reste consultable dans l'historique git
(jusqu'au commit `c8afe43`).

| Étape | Contenu | État |
|-------|---------|------|
| 1 | `lunii-core` (port de `pkg/api`) + `lunii-cli` | ✅ fait |
| 2 | `lunii-gui` : interface [Slint](https://slint.dev) (port de `pkg/ui`, `main_window.py`, workers) | ✅ fait |
| 3 | Suppression du code Python | ✅ fait |
| 4 | Packaging et CI (Windows / macOS / Linux) | à faire |

## Organisation

```
rust/
├── lunii-core/        bibliothèque, sans dépendance à une interface
│   ├── src/constants.rs     constantes, répertoire ~/.lunii-qt
│   ├── src/crypto.rs        XXTEA (rounds Lunii), AES-CBC
│   ├── src/db.rs            bases officielle / tierce, cache des vignettes
│   ├── src/story.rs         Story, StoryList, STUdio, détection des archives
│   ├── src/archive.rs       lecture uniforme zip / 7z
│   ├── src/convert/         images (BMP RLE4, LIFF) et audio (MP3, ffmpeg)
│   ├── src/device/          trait Device, Lunii v1/v2/v3, Flam
│   ├── src/firmware.rs      téléchargement des firmwares
│   ├── src/tools.rs         import base STUdio, ffmpeg, mise à jour
│   └── tests/               scénarios complets sur de faux appareils
├── lunii-cli/         outil en ligne de commande
└── lunii-gui/         interface graphique Slint (binaire `lunii-qt`)
    ├── ui/                  écrans .slint (fenêtre principale, liste, dialogues, journal)
    ├── src/app.rs           contrôleur (port de main_window.py / nm_window.py / debug_ui.py)
    ├── src/worker.rs        tâches longues en arrière-plan (port de ierWorker.py)
    ├── src/i18n.rs          traductions des messages côté Rust
    ├── src/logfile.rs       fichier de log persistant et logger de la façade `log`
    ├── lang/fr/…/lunii-gui.po  traduction française (générée)
    └── tools/ts2po.py       conversion de locales/fr_FR.ts vers .po
```

Les signaux Qt des appareils Python sont remplacés par le trait `Reporter`
(logs + progression), et `abort_process` par `Context::request_abort()`, ce qui
permettra à l'interface Slint de lancer les opérations dans un thread.

Le répertoire de configuration reste `~/.lunii-qt` : bases, cache, clés v3 et
sauvegardes `.md` / `.mdf` sont partagés avec la version Python.

## Compilation

```bash
cd rust
cargo build --release
cargo test
```

Les binaires sont `target/release/lunii-qt` (interface) et `target/release/lunii-cli`. ffmpeg reste nécessaire pour
transcoder l'audio des histoires STUdio (recherché dans `~/.lunii-qt` puis dans le `PATH`).

## Interface graphique

```bash
cargo run --release -p lunii-gui
```

Elle reprend la fenêtre Qt : sélection de l'appareil, filtre, liste des
histoires (sélection multiple Ctrl/Maj, flèches, Ctrl+A, menu contextuel),
vignette et description, barres de progression avec bouton d'arrêt, barre
d'état (un clic sur le SNU copie les détails de l'appareil), menus et raccourcis
(Ctrl+I, Ctrl+S, Suppr, Alt+↑/↓, Ctrl+↑/↓, Ctrl+H, Ctrl+N, Ctrl+L, F5, F1…),
réglages du mode nuit, connexion Luniistore pour le firmware, journal de debug,
vérification des mises à jour, glisser-déposer d'archives.

La langue suit le système (français ou anglais) ; `LUNII_QT_LANG=en` ou
`LUNII_QT_LANG=fr` la force.

Variables utiles au développement :

| Variable | Effet |
|----------|-------|
| `LUNII_QT_DEVICES=/chemin/a:/chemin/b` | remplace la détection des appareils (faux appareils de test) |
| `LUNII_QT_NO_UPDATE_CHECK=1` | pas de requête vers GitHub au démarrage |
| `LUNII_QT_WAYLAND=1` | garde le backend Wayland natif au lieu de X11 (XWayland) |
| `LUNII_QT_CFG_DIR=…` | autre répertoire de configuration que `~/.lunii-qt` |

Captures d'écran sans affichage (rendu logiciel), avec la feature `screenshot` :

```bash
cargo build -p lunii-gui --features screenshot
LUNII_QT_DEVICES=/chemin/faux_lunii LUNII_QT_NO_UPDATE_CHECK=1 \
LUNII_QT_SCREENSHOT=/tmp/captures target/debug/lunii-qt
```

### Traductions

Les textes de l'interface Slint (`@tr(...)`) et les messages produits en Rust
utilisent le même fichier `lang/fr/LC_MESSAGES/lunii-gui.po`. Il est généré à
partir de la traduction Qt existante, plus `tools/extra_fr.po` pour les textes
propres à la version Rust :

```bash
cd lunii-gui
python3 tools/ts2po.py ../../locales/fr_FR.ts tools/extra_fr.po > lang/fr/LC_MESSAGES/lunii-gui.po
```

Les messages de `lunii-core` (en anglais) sont traduits à l'affichage en les
comparant aux modèles `{}` du catalogue (« Missing {} in {} »).

### Différences avec l'interface Qt

- **Glisser-déposer** : il fonctionne sous Windows, macOS et X11. winit 0.30 ne
  le gère pas sous Wayland natif. Dans une session Wayland où XWayland est
  disponible (`DISPLAY` défini), l'appli utilise donc X11 automatiquement
  (`LUNII_QT_WAYLAND=1` force le Wayland natif, sans glisser-déposer).
- **Journal** : au lieu d'une fenêtre séparée, c'est un panneau repliable en bas
  de la fenêtre principale (Ctrl+L, ou l'icône à droite de la barre d'état).
  On le redimensionne avec la poignée au-dessus. L'icône affiche un point orange
  ou rouge si un avertissement ou une erreur est arrivé pendant que le panneau
  était fermé. Le bouton « Copier » met les lignes affichées (filtrées) dans le
  presse-papiers.
  Chaque session est aussi écrite dans `~/.lunii-qt/logs/lunii-qt.log`, avec un
  horodatage et sans les lignes de progression. Les quatre sessions précédentes
  sont gardées (`lunii-qt.log.1` à `.4`), et le bouton « Ouvrir le dossier des
  journaux » ouvre ce dossier.
- **Démarrage** : la base officielle est chargée en arrière-plan. La fenêtre
  s'ouvre tout de suite et les noms s'affichent dès que la base est prête.
- **Rafraîchissement de la base** et **téléchargement du firmware** : ils
  tournent en arrière-plan au lieu de bloquer l'interface.
- **Raccourci F2** (déblocage de l'export des histoires officielles) : il n'est
  pas repris.
- **Factory reset** : l'entrée était cachée et non implémentée en Python ; elle
  n'est pas reprise.
- **Mode nuit d'une histoire masquée** : il est conservé à la réouverture de
  l'appareil. Python le cherchait dans `.content/` au lieu de `.content.hidden/`.

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

Comme l'application Python, l'ouverture d'un appareil réécrit son index
(`.pi` / `etc/library/list`).

## Différences avec la version Python

Le comportement a été vérifié octet par octet contre le code Python d'origine :
mêmes fichiers sur l'appareil et mêmes exports pour Lunii v2, Lunii v3 et Flam,
avec des archives plain.pk, v2 (zip et 7z), STUdio (conversion d'images,
suppression des tags, transcodage ffmpeg) et Flam plain.

Écarts volontaires, qui corrigent des bugs du code Python :

- **Restauration d'une sauvegarde Lunii v3** : Python écrivait l'histoire dans
  `.content/<UUID complet>/` au lieu de `.content/<8 derniers caractères>/`.
- **Images STUdio au format BMP** : Python les convertissait à l'envers (il ne
  retournait que les sources non BMP, alors que Pillow décode déjà les BMP dans
  le bon sens).
- **Export des histoires officielles** : refusé dans la bibliothèque elle-même
  pour les Lunii v1/v2. Python ne le bloquait que dans l'interface, et seulement
  quand une seule histoire était sélectionnée. Sur Lunii v3 et sur Flam, une
  histoire officielle n'est exportée que sous forme de sauvegarde chiffrée, à
  remettre sur un appareil, jamais déchiffrée. Python l'exportait en clair dès
  que les clés étaient connues.
- **Récupération des histoires perdues** : Python ne trouvait rien si
  `.content.hidden/` n'existait pas.
- **Vignette Flam manquante** : avertissement au lieu d'un plantage.
- **Archive STUdio en 7z** : les tags ID3 sont supprimés, comme pour le zip.
- **MP3 illisible** dans une archive STUdio : transcodé par ffmpeg au lieu de
  faire échouer l'import.
- **Mode nuit sur une histoire masquée** : agit sur le bon dossier.
- **Export interrompu ou en erreur** : l'archive partielle est supprimée.

Le redimensionnement des images (bicubique) n'est pas identique au bit près à
Pillow (écart moyen mesuré inférieur à 0,1/255).

Les messages de la bibliothèque sont en anglais. La traduction (`locales/fr_FR.ts`)
sera reprise avec l'interface Slint.
