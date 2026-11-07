# MKV → Atmos

Petite appli Mac : on glisse un MKV qui contient de l'Atmos **Dolby TrueHD** (lossless), par exemple un album Headphone Dust ou un rip de Blu-ray, et elle crée :

- un **ADM BWF** (`… - ADM.wav`) : le master Atmos avec ses objets et leurs mouvements. Il se lit dans le standalone **Virtuoso**, s'importe dans **Pro Tools** (Import Session Data) et s'ouvre dans le **Dolby Atmos Renderer** ;
- un **WAV 7.1.4** (`… - 7.1.4.wav`) : un rendu en 12 canaux qui se lit partout (Virtuoso, Pro Tools, Reaper…). Ordre des canaux : L R C LFE Lss Rss Lrs Rrs Ltf Rtf Ltr Rtr.

- en option, un **master Dolby `.atmos` (DAMF)**, qui s'ouvre dans le Dolby Atmos Renderer.

Option : **un fichier par chapitre**, donc un fichier par morceau quand le MKV a des chapitres (ADM, 7.1.4 et `.atmos`).

Pendant la conversion, l'appli affiche une barre de progression globale avec le temps restant estimé, et un journal de chaque étape. Le journal est aussi enregistré dans `journal.txt`, dans le dossier de sortie : c'est le fichier à envoyer si quelque chose plante.

## Installer

1. Va dans **Releases** (colonne de droite sur GitHub), télécharge `MkvToAtmos.zip`.
2. Dézippe et glisse **MkvToAtmos** dans **Applications**.
3. Première ouverture : l'appli n'est pas signée par Apple, macOS la bloque. Fais **clic droit > Ouvrir**, puis **Ouvrir**. Si macOS refuse encore : **Réglages Système > Confidentialité et sécurité**, tout en bas, **Ouvrir quand même**.

Ensuite, double-clic normal.

## Utiliser

1. Ouvre l'appli et glisse un ou plusieurs `.mkv` dans la fenêtre (ou clique pour en choisir).
2. Coche ce que tu veux créer.
3. Les fichiers arrivent dans un dossier `NomDuMKV - Atmos`, à côté du MKV. Le bouton **Afficher dans le Finder** y mène directement.

Si le MKV contient plusieurs pistes TrueHD (par exemple l'album et sa version instrumentale), chacune est convertie, avec le nom de la piste dans le nom du fichier.

Compte environ 2 fois la taille du MKV en espace disque libre pendant la conversion.

## Ce qu'il faut savoir

- **C'est lossless.** L'audio de l'ADM est copié bit pour bit depuis le TrueHD décodé.
- **Ce n'est pas la session du mixeur.** Dans un TrueHD Atmos, les objets sont regroupés en une quinzaine d'éléments (clusters) et le bed ne contient en général que le LFE. C'est le même contenu qu'un lecteur Blu-ray envoie à un ampli Atmos.
- **Le WAV 7.1.4 est un rendu maison** (panoramique cartésien comme le renderer Dolby), validé contre le renderer de référence de l'UER (EAR). Pour un rendu Dolby exact, utilise l'ADM dans un renderer Dolby.
- **Chapitres décalés** : certains MKV ont des chapitres mal placés (c'est le cas de *The Harmony Codex* chez Headphone Dust, décalage de 40 s). Dans ce cas, décoche « Un fichier par chapitre » ou corrige les chapitres avec MKVToolNix.

## Sous le capot

- `core/` : outil en ligne de commande `atmosconv` (Rust, sans dépendance). Il extrait la piste TrueHD du MKV, la décode avec [truehdd](https://github.com/truehdd/truehdd), puis écrit l'ADM BWF (BW64 au-delà de 4 Go) et le rendu 7.1.4.
- `app/` : l'interface SwiftUI.
- `.github/workflows/build.yml` : compile l'appli universelle (Apple Silicon et Intel) sur un Mac de GitHub et publie le zip dans Releases à chaque modification.

En ligne de commande :

```
atmosconv convert album.mkv --chapters
atmosconv tracks album.mkv          # liste pistes et chapitres
atmosconv damf master.atmos         # part d'un master DAMF existant
```

Le fichier tests/atmos_test.mkv est construit à partir des échantillons de test de truehdd (Apache-2.0).
