# Idées reportées

Pistes repérées (audit du code de novembre 2026, ou en cours de route) et
laissées pour plus tard. Retirer une entrée une fois traitée.

## Interface

- **Découper `App::connect()`** (`app/mod.rs`, ~160 lignes) par écran : liste,
  podcasts, dialogues. Même idée pour les branches longues de
  `App::action()` (`app/actions.rs`, ~200 lignes).
- **État de l'interface en « état + messages »** : `App` modifie
  `RefCell<State>` et la fenêtre depuis chaque gestionnaire. Une fonction
  `update(état, message)` sans Slint rendrait l'interface testable. Les
  tickets (`tickets.rs`) et le thread de l'appareil (`device_actor.rs`) en
  sont la première étape : l'interface ne lit plus l'appareil.
- **Âge conseillé des podcasts** : aucun flux RSS ne le donne. Les pages
  Radio France l'ont dans `<meta name="ad:rubriques">`
  (`podcasts-enfants-a-partir-de-{3,6,9}-ans`, parfois plusieurs : garder le
  plus petit). Ailleurs, seulement du texte libre (« dès 5 ans »,
  « ages 4-10 » dans `itunes:keywords`), rare et peu fiable.

## Erreurs ignorées

- `app/podcast.rs:904` ignore l'échec de `podcast::follow` (la ligne 562 le
  journalise).
- Les `open::that` (dossier de l'appareil, des journaux, page des versions)
  échouent sans message si aucun programme ne peut ouvrir la cible.

## Appareils

- **Opérations calculées puis exécutées** : chaque opération (import,
  modification d'album, suppression) calculerait d'abord la liste des
  fichiers à écrire, renommer et supprimer, puis l'exécuterait. On en
  tirerait un vrai `--dry-run`, un total d'octets exact pour la progression
  et des tests sans écriture. La modification d'album prépare déjà ses
  fichiers dans un dossier temporaire.
- **Rembourrage sur Flam** : une histoire Lunii mise sur une Flam a ses
  `ri` / `si` / `li` complétés à un multiple de 16 octets (AES) ; l'export en
  clair depuis la Flam garde ce rembourrage (test
  `v2_archive_imports_on_lunii_and_flam`). À vérifier sur une vraie Lunii :
  si elle refuse ces fichiers, tronquer à la taille d'origine à l'export.
- **Sauvegarde chiffrée des histoires officielles sur Lunii v1/v2** : seul
  modèle où elles ne peuvent pas être sauvegardées du tout (la Flam et la v3
  font une sauvegarde chiffrée, restaurable sur le même appareil).
  `common::export_backup_story` existe déjà ; reste à vérifier l'import de ces
  sauvegardes, avec un test d'aller-retour.

## Albums

- **Deux constructions parallèles de `story.json`** (`album/layout.rs`) :
  `story_json` et `chapters_story_json` répètent la couverture, les nœuds de
  lecture et l'enveloppe. Les fusionner demande de garder exactement le même
  ordre des nœuds (les fichiers d'index de l'appareil en dépendent).

## Réseau

- **Proxy** : seulement les variables d'environnement, et le proxy système
  sous Windows (`http.rs`). Restent : un réglage dans l'interface, le proxy
  système de macOS et de GNOME/KDE, les fichiers PAC, et le format Windows
  par protocole (`http=hôte:port;https=…`), que `ureq` ne lit pas.

## Tests

- **CLI** : aucun test. Quelques parcours de bout en bout sur une fausse Lunii
  (`list`, `import` d'une archive et d'un album, `export`, `remove`).
