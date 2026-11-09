# MCO Dash

Dashboard de bureau (Rust / egui) : tickets Freshservice (groupe MCO) + supervision Centreon.

## Config

Renseigner Centreon et Freshservice dans l'onglet **Paramètres** (stockés dans `~/.config/mco-dash/settings.json`). Un `.env` (voir `.env.example`) peut fournir des valeurs par défaut.

## Lancer

```bash
cargo run --release
```

## Sécurité des paramètres

Mot de passe Centreon et clé API Freshservice : chiffrés dans `settings.json` sous Windows (DPAPI, liés à votre
compte Windows ; le fichier copié ailleurs est illisible, il faut ressaisir les secrets). Sous Linux/WSL, pas de
coffre universel : le fichier reste en clair mais en `0600` dans un dossier `0700`. Un `.env` éventuel reste en clair.

## Mode discret

Bouton « Mode discret » en haut à droite : la fenêtre devient une petite barre toujours visible (compteurs
Centreon / Freshservice) qui s'agrandit d'une ligne quand un ticket non assigné apparaît ou qu'un de vos
tickets est mis à jour (boutons Ouvrir / Vu, la barre des tâches clignote). Glisser la barre pour la déplacer ;
le bouton d'agrandissement ramène la fenêtre complète. L'épingle (bleue = active) garde la barre au premier
plan ; la désépingler la laisse passer derrière les autres fenêtres. Le mode et l'épingle sont mémorisés.

## Sons

Un son distinct est joué pour un nouveau ticket non assigné et pour une mise à jour de vos tickets. Le haut-parleur
(coin bas droit, ou dans la barre discrète) coupe / rétablit les sons ; Paramètres > Sons de notification règle le volume
et le son de chaque type (avec bouton Tester). Sons synthétisés, sans fichier audio ; lecture via PulseAudio sous
Linux/WSL (`libpulse-simple`) et winmm sous Windows.

Quand la fenêtre n'est pas au premier plan, la notification est aussi envoyée au système (toast Windows, ou
D-Bus sous Linux : il faut un démon de notifications, absent de WSLg par défaut). Désactivable dans Paramètres >
Notifications. Inutile avec la barre discrète épinglée, déjà visible.

## Windows (.exe)

Compilation croisée depuis WSL/Linux :

```bash
rustup target add x86_64-pc-windows-gnu
sudo apt install gcc-mingw-w64-x86-64
cargo build --release --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/mco-dash.exe
```

Les paramètres sont alors dans `%APPDATA%\mco-dash\settings.json`.
