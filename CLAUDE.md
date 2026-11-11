# Instructions pour les agents IA

Application Rust (interface Slint) pour gérer les histoires des conteuses Lunii
et Flam. Le guide complet est [`CONTRIBUTING.md`](CONTRIBUTING.md) :
organisation du code, prérequis, variables d'environnement, traductions. Le
lire avant un changement non trivial ; ce fichier n'en reprend que l'essentiel.

## Vérifications

Avant de proposer un commit, les mêmes que la CI, depuis la racine :

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check -p lunii-gui --features screenshot --locked
cargo test --workspace --locked
```

## Ne jamais toucher aux données réelles

- Pas de lecture/écriture dans `~/.lunii` : passer par `LUNII_CFG_DIR=<dossier
  temporaire>` (ou `set_cfg_dir` dans les tests).
- Pas d'accès à une vraie Lunii/Flam : `LUNII_DEVICES=<faux appareil>`.
  `tools/fake-lunii.sh` en crée un (Lunii v2, histoires inventées) et lance
  l'application dessus.
- Pas de requête réseau superflue : `LUNII_NO_UPDATE_CHECK=1`.

## Vérifier un changement d'interface

Pas d'affichage nécessaire : la feature `screenshot` rend les écrans en PNG
(rendu logiciel, séquence dans `lunii-gui/src/screenshot.rs`). Regarder les
PNG produits avant d'annoncer qu'un changement d'interface fonctionne.

```bash
tools/fake-lunii.sh --no-run /tmp/fake
cargo build -p lunii-gui --features screenshot
LUNII_DEVICES=/tmp/fake/lunii LUNII_CFG_DIR=/tmp/fake/cfg LUNII_NO_UPDATE_CHECK=1 \
LUNII_LANG=fr LUNII_SCREENSHOT=/tmp/shots target/debug/lunii
```

Les captures du README se refont avec `tools/screenshots.sh` (Python 3 et
Pillow, pour l'image annotée).

## Conventions

- Commentaires et messages de `lunii-core` en anglais (traduits à
  l'affichage). Tout texte affiché passe par `@tr()` (Slint) ou `tr()`/`trf()`
  (Rust), avec son entrée dans `lunii-gui/lang/fr/LC_MESSAGES/lunii-gui.po`.
- Documentation (README, CONTRIBUTING) en français ; `README_EN.md` suit
  `README.md`.
- Après un changement de dépendance : `cargo build`, puis commiter
  `Cargo.lock`.

## Idées reportées

[`TODO.md`](TODO.md) liste les pistes laissées pour plus tard. Y ajouter
celles qu'on repère sans les traiter ; retirer une entrée une fois faite.

## Commits

- En français, préfixe conventionnel : `feat:`, `fix:`, `docs:`, `refactor:`…
  avec une portée si utile (`fix(podcasts): …`).
- Succincts : un titre clair sur l'intention ; au plus une ligne de corps si
  le pourquoi n'est pas évident. Pas de détail de l'implémentation.
- Pas de ligne `Claude-Session` dans les messages.
- Ne pas pousser ni réécrire l'historique publié sans accord explicite.
