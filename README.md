# irc-xdcc

Client IRC pour Windows (et Linux) dans l'esprit de **mIRC**, avec une gestion complète des
**XDCC** : file d'attente par bot, DCC actif et **passif**, reprise des fichiers interrompus,
sons de début, de fin et d'échec. Police **Fixedsys Excelsior** (les deux variantes) ou une autre
au choix, thème clair aux couleurs de mIRC.
Un seul `.exe` autonome, sans DLL ni installation.

## Compiler

```sh
./build.sh                # -> dist/irc-xdcc.exe  (Windows x64, cross-compilé)
./build.sh linux          # -> dist/irc-xdcc      (Linux)
./build.sh test           # tests
./build.sh cargo clippy   # n'importe quelle commande cargo
```

- **Avec Docker** (utilisé s'il est installé) : rien d'autre à installer. Le `Dockerfile` part
  de `rust:latest` et utilise MinGW ; le cache cargo est dans le volume `irc-xdcc-cargo`.
- **Sans Docker** : il faut [rustup](https://rustup.rs) avec la cible
  `x86_64-pc-windows-gnu`, [zig](https://ziglang.org) et
  [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) dans le `PATH` ; zig sert
  d'éditeur de liens pour Windows.

Les tests comprennent un scénario complet contre un faux serveur IRC et un faux bot XDCC
(pseudo de secours, perform, favoris, file, DCC actif, passif, reprise, fichier déjà complet, bot
absent et envoi tardif, file d'attente du bot, fenêtre des messages, réponses CTCP, mode
invisible, pas d'ouverture en double) et un passage de toute l'interface sans fenêtre.
`cargo test -- --ignored` vérifie en plus la connexion TLS à un vrai serveur (réseau requis).

`cargo deny check` (avec [cargo-deny](https://github.com/EmbarkStudios/cargo-deny), réglé par
`deny.toml`) vérifie les dépendances : licences compatibles, failles connues (base RustSec),
provenance (crates.io uniquement).

## Intégration continue et versions

Sur GitHub, `.github/workflows/ci.yml` vérifie chaque envoi et chaque demande de fusion (format,
clippy pour Linux et Windows, tests, `cargo deny`), puis compile les deux exécutables avec zig,
téléchargeables dans les artefacts de l'exécution. Pour publier une version, au choix :

- **à la demande** : onglet *Actions*, workflow *CI*, *Run workflow*, en indiquant le numéro
  (`1.2.3`, ou `1.2.3-beta.1` pour une préversion) ; l'étiquette `v1.2.3` est créée sur le
  commit compilé ;
- **par une étiquette** : `git tag v1.2.3 && git push origin v1.2.3` ;
- **depuis GitHub** : *Releases*, *Draft a new release*, avec une nouvelle étiquette `v1.2.3` ;
  à la publication, les exécutables sont compilés puis ajoutés à cette version (quelques
  minutes), qui garde le titre, les notes et la case préversion choisis.

Le numéro donné remplace celui de `Cargo.toml` dans les exécutables (menu *Aide*, réponse
complète à VERSION proposée dans les options) ; celui du dépôt ne sert qu'aux compilations
locales. La version GitHub contient `irc-xdcc-windows-x86_64.exe`, `irc-xdcc-linux-x86_64`
(à rendre exécutable : `chmod +x`) et leurs sommes de contrôle (`SHA256SUMS`) ; créée par la CI,
elle a des notes tirées des commits. L'exécutable Linux ne demande que la glibc 2.17 (2012) : il démarre sur
toutes les distributions actuelles, et même bien plus anciennes.

## Licence

Au choix, [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE-APACHE), comme l'essentiel de l'écosystème
Rust. Les polices intégrées gardent leur licence : Fixedsys Excelsior est dans le domaine
public ; les polices d'egui (Hack, Ubuntu, Noto Emoji…) sont sous licences libres (OFL, Ubuntu
Font Licence…).

## Utilisation

Au premier lancement, la fenêtre **Options** s'ouvre sur l'identité : choisir un pseudo.

- **Se connecter** : menu *Serveurs* → un réseau, ou *Autre serveur…* ; ou `/server hôte:+6697`
  (`+` = TLS ; `/server -m …` ouvre une nouvelle fenêtre de serveur). Un réseau, un serveur ou
  un canal déjà ouvert n'est jamais rouvert en double : sa fenêtre est affichée (et reconnectée si
  besoin).
- **Fenêtres** : arborescence à gauche (serveurs, canaux, conversations privées, transferts),
  colorée selon ce qui n'a pas été lu, comme dans mIRC : bleu pour de nouvelles lignes sans
  message (arrivées, modes…), rouge pour au moins un nouveau message, rose pour un message qui
  vous concerne. En revenant dans une fenêtre où de nouvelles lignes sont arrivées, un trait
  gris les sépare de ce qui avait déjà été lu ; la position de défilement est celle où on l'avait
  laissée (tout en bas, la vue suit les nouvelles lignes). La **fenêtre active** est celle
  affichée dans la fenêtre principale, ou celle ouverte à part qui a le focus : une fenêtre
  ouverte à part sans le focus se colore dans l'arborescence et reçoit le trait comme une
  fenêtre quittée, et le logiciel ne clignote dans la barre des tâches que si aucune de ses
  fenêtres n'a le focus. Une conversation privée suit son correspondant quand il change de
  pseudo : elle reste affichée là où elle l'était, avec le texte en cours de saisie. Clic droit : se connecter, se
  reconnecter, se déconnecter (serveur), quitter ou rejoindre (canal), fermer, ajouter aux
  favoris, **ouvrir dans une fenêtre séparée** (une vraie fenêtre Windows, déplaçable sur un
  autre écran ; une fois ouverte, l'action devient « Afficher la fenêtre séparée », qui la met
  au premier plan). Un clic dans l'arborescence, comme les raccourcis qui suivent, affiche la
  fenêtre dans la fenêtre principale, même si elle est aussi ouverte à part.
  `Ctrl+Page suivante` / `Ctrl+Page précédente` ou `Ctrl+Tab` / `Ctrl+Shift+Tab` :
  fenêtre suivante / précédente dans l'arborescence ; `Alt+←` / `Alt+→` : fenêtre visitée
  avant / après, comme dans un navigateur (une fenêtre fermée quitte cet historique). `Ctrl+W` : fermer la
  fenêtre affichée (celle d'un serveur le déconnecte, ferme ses
  canaux et annule ses transferts) ; la précédente dans l'arborescence est alors affichée. Dans
  une fenêtre séparée, `Ctrl+W` ferme seulement cette fenêtre : le canal reste ouvert dans
  l'arborescence.
- **Boîtes de dialogue** : `F1` ouvre l'aide, `Alt+O` les options ; `Échap` ou `Alt+F4` ferment
  celle du dessus (aide, options, autre serveur, erreur) : `Alt+F4` ne quitte le logiciel
  qu'une fois toutes fermées.
- **Liste des pseudos** : double-clic → conversation privée ; clic droit → whois, version,
  liste XDCC, op/voice, expulsion.
- **Liens** : un clic sur une adresse web l'ouvre dans le navigateur par défaut (nouvel onglet
  du navigateur déjà ouvert), un clic sur un `#canal` le rejoint, y compris précédé d'un rang
  comme dans un /whois (`@#canal`, `+#canal`, `%#canal`).
- **Défilement** : `Page préc.` / `Page suiv.` d'une page, `Ctrl+↑` / `Ctrl+↓` d'une ligne,
  `Ctrl+L` jusqu'au trait des lignes lues, `Ctrl+Début` / `Ctrl+Fin` tout en haut / tout en bas
  (la vue suit alors de nouveau les nouvelles lignes) quand la saisie est vide, sinon ils y
  déplacent le curseur ; dans la fenêtre principale ou la fenêtre séparée qui a le focus. Avec `Shift` en
  plus, ces touches sélectionnent dans la saisie, comme d'habitude.
- **Saisie** : `Entrée` envoie, `Ctrl+Entrée` envoie comme du texte même ce qui commence par `/`,
  `Tab` complète les pseudos, `↑` `↓` rappellent l'historique, `Ctrl+←` / `Ctrl+→` sautent de mot
  en mot, `Ctrl+Z` annule, `Ctrl+Y` ou `Ctrl+Shift+Z` rétablit, `Ctrl+K` couleur, `Ctrl+B` gras
  (Fixedsys n'ayant pas de variante grasse, le texte est épaissi d'un pixel, comme Windows le
  fait pour les polices bitmap), `Ctrl+U` souligné, `Ctrl+I` italique, `Ctrl+O` texte normal.
  `/help` (ou *Aide → Commandes*) ouvre la fenêtre d'aide, comme dans mIRC : les commandes
  (`/join`, `/part`, `/msg`, `/query`, `/me`, `/nick`, `/topic`, `/kick`, `/whois`, `/ns`, `/cs`,
  `/raw`…) et les raccourcis ; une commande inconnue est
  envoyée telle quelle au serveur, comme dans mIRC. Comme dans mIRC, un message envoyé s'affiche
  (`-> *Bot* xdcc send #1`).
- **Texte** : comme dans mIRC, glisser la souris sélectionne le texte (affiché en inversé, la
  vue défile si on sort de la fenêtre) ; au relâchement il est copié dans le presse-papiers et la
  sélection disparaît. Un texte de la couleur du fond (blanc sur blanc, noir sur noir…) est
  affiché dans une couleur lisible, là où mIRC le laisse invisible. Les caractères sans dessin
  (espace sans chasse glissé dans un pseudo…) ne laissent pas de blanc, comme dans mIRC. Les
  lignes reçues sont lues en UTF-8, et en Windows-1252 pour les seuls octets invalides (anciens
  clients) : une ligne qui mélange les deux garde ses accents. Dans la fenêtre d'état, comme dans
  mIRC, chaque évènement (notice, message du logiciel, réception DCC…) est encadré de lignes
  « - », et les réponses du serveur d'un même groupe (accueil, statistiques, compteurs
  d'utilisateurs, message du jour) restent ensemble. Les lignes suivent une même règle, celle de
  mIRC :
  - sans « * », en couleur normale : le texte du serveur tel quel, refus compris
    (« UnknownNick No such nick/channel »), sauf son message de fin (« Closing Link… ») en vert ;
    les messages des utilisateurs (`<pseudo>`, `-pseudo-`, et `->` pour ce qu'on envoie) ;
  - sans « * », en rouge : les échanges CTCP et DCC (« [Nick VERSION] », offre, envoi ignoré,
    « Réception DCC de fichier depuis Bot terminée (00:00:09, 556 Kio/s) », échouée…) ;
  - sans « * », en vert : l'adresse publique trouvée (« Hôte local : hôte (adresse IP) », le nom
    étant cherché par DNS inverse quand le serveur ne donne que l'adresse) ;
  - avec « * », en bleu : les autres messages du logiciel (« * Connexion… », « * Déconnecté »,
    « * Usage : … », « * /msg : non connecté au serveur », « * UPnP : … », même en échec, car
    le transfert peut réussir quand même) ; en rouge s'il s'agit d'un échec (SASL, aucun pseudo
    accepté, bot injoignable, adresse locale annoncée en DCC passif) ;
  - avec « * », dans leurs couleurs : les arrivées, départs, modes (« * NickServ met le mode :
    +r »), sujets et actions (`/me`).

  La réponse à un `/whois` est mise en forme (« Nick est
  user@hôte * nom réel », « Nick est sur #canal », « Nick est inactif depuis 1 min 03 s,
  connecté le … ») ; comme la réponse à toute autre commande (`/who`, `/list`…), ses lignes
  restent groupées, avec un séparateur après « End of … ».
- Une fenêtre réduite continue de tout recevoir ; un message qui vous concerne (pseudo, mot
  surveillé, message privé) fait clignoter le bouton de la barre des tâches.

### Options

| Onglet | Contenu |
|---|---|
| Identité | pseudo, **pseudo de secours** (s'il est pris ; ensuite `pseudo_`, `pseudo__`…), ident, nom réel, message de départ, **réponse à VERSION** |
| Connexion | reconnexion automatique et délai, retour dans les canaux après reconnexion, **serveur ident**, **mode invisible** (+i), mots mis en évidence |
| Serveurs | réseaux (dans l'ordre du menu *Serveurs*, réordonnables ▲ ▼) : serveurs (hôte, **port**, TLS, certificat non vérifié, essayés dans l'ordre choisi), connexion au démarrage, pseudo propre au réseau, mot de passe serveur, **SASL**, **commandes à la connexion** (perform, ex. `/ns identify motdepasse`, le mot de passe masqué dans l'écho) et délai avant d'entrer dans les canaux |
| Favoris | canaux favoris (dans l'ordre du menu *Favoris*, réordonnables ▲ ▼), chacun avec sa clé, son réseau et son serveur : **automatique** (ceux du réseau, à tour de rôle), l'un des serveurs du réseau, ou un autre saisi comme `hôte:+port` (TLS) ; entrée automatique |
| DCC | dossier de téléchargement (local, lecteur réseau ou chemin réseau comme `\\NAS\partage\XDCC`), envoi non sollicité (afficher la demande / accepter automatiquement / tout ignorer), **si le fichier existe** (liste : demander / **reprendre** / écraser / renommer), **liste d'ignorés** désactivable (refuser ces fichiers / n'accepter que ces fichiers ; jokers, pseudos), plage de ports, IP et **UPnP** pour le DCC passif, délai sans données |
| XDCC | nouvelles tentatives, délai de réponse du bot |
| Sons | interrupteur général ; bip à chaque message (canal, conversation privée, fenêtre active remontée dans l'historique), comme *Beep on message* de mIRC ; début d'un transfert, transfert terminé, échoué, bot injoignable (absent du réseau, parti, muet), déconnexion du serveur : chacun activable, avec un fichier audio au choix (WAV, MP3…) ou le son du système, et un bouton pour l'écouter ; les sons se suivent, jamais l'un par-dessus l'autre |
| Affichage | **langue** (automatique : celle du système, français ou anglais, et l'anglais pour toute autre langue ; English (United States) ; Français (France)), police (Fixedsys Excelsior standard ou ALT, ou un fichier `.ttf`/`.otf`, avec raccourcis vers Consolas, Lucida Console… sous Windows), taille, thème clair (couleurs de mIRC) ou sombre, horodatage, arrivées/départs, couleurs du texte, historique |

Menu *Favoris* : ajouter le canal affiché, rejoindre un favori (en se connectant à son serveur
si besoin). Les favoris « auto » sont rejoints à chaque connexion à leur réseau / serveur, après
le perform et le délai choisi : cliquer sur un favori d'un réseau pas encore connecté ouvre donc
aussi les favoris « auto » de ce réseau. Si certains n'ont pas pu être rejoints (canal réservé
aux pseudos identifiés, plein…), demander de nouveau ce serveur (menu *Serveurs*, *Autre
serveur…*, `/server -m`, un de ses favoris) rejoint les manquants ; le reconnecter le fait aussi.

**Discrétion** : par défaut, rien ne dit quel logiciel est utilisé. Nom d'utilisateur `user`,
nom réel identique, pas de message de départ, aucune réponse à `/ctcp VERSION` (le bouton *Ce
logiciel* des options remplit la réponse complète) ; `/ctcp TIME` reçoit l'heure en UTC, sans le
fuseau horaire. Le **mode invisible** (+i, activé par défaut) est demandé à chaque connexion :
absent de `/who` et des listes des utilisateurs pour qui ne partage aucun canal avec vous. Le
logiciel ne contacte que les serveurs IRC, les émetteurs DCC, la box (UPnP)
et le DNS (adresse publique pour le DCC passif) : ni mise à jour, ni statistiques.

**Serveur ident** (activé par défaut, comme dans mIRC) : à la connexion, le serveur IRC demande
au port 113 du PC le nom d'utilisateur ; sans réponse, il affiche *No ident response* et le
préfixe d'un `~`. C'est sans conséquence ; pour l'éviter, le port 113 doit être redirigé vers le
PC sur la box : automatiquement par **UPnP** (*Options → DCC*, activé par défaut ; redirection
d'une heure, renouvelée tant que le logiciel tourne), sinon à la main.

Un fichier de configuration d'une version précédente reste lisible : les nouveaux réglages
prennent leur valeur par défaut, ceux qui ont disparu sont ignorés.

La configuration est un fichier TOML lisible : `%APPDATA%\irc-xdcc\config.toml` (Windows) ou
`~/.config/irc-xdcc/config.toml`. **Mode portable** : un fichier `irc-xdcc.toml` à côté de
l'exécutable est utilisé à la place.

## XDCC

```
/xdcc Bot #12                 un pack
/xdcc Bot #1,#4,#10-#15       plusieurs packs
/msg Bot xdcc send #12        même chose (la demande passe aussi par la file)
/xdcc list Bot                liste des packs du bot
/xdcc cancel                  annule la file du serveur
```

- Les packs entrent dans une **file**, comme dans XDCC Klipper : un seul pack demandé à la fois
  par bot (le suivant quand il est terminé), tous les bots en même temps. C'est le bot qui gère sa
  propre file : la position qu'il annonce s'affiche ; s'il est plein (*try again later*), le pack
  est redemandé une à deux minutes plus tard ; sans nouvelles de sa file pendant une heure, il est
  redemandé. Les demandes ne partent qu'une fois identifié et entré dans les canaux favoris :
  beaucoup de bots l'exigent.
- L'envoi du bot est accepté automatiquement. Si le fichier existe déjà, il est **repris**
  (`DCC RESUME`) par défaut. Un fichier déjà complet n'est pas retéléchargé : la reprise est
  demandée sur son dernier octet (les bots refusent une reprise à la fin même), que le bot
  renvoie à l'identique avant de clôturer son envoi, au lieu de le laisser en attente (« Déjà
  téléchargé », avec les sons de début et de fin de cette courte connexion). Chaque transfert ne
  se termine qu'une fois : un seul message et un seul son de fin ou d'échec.
- **DCC passif** (le bot ne peut pas recevoir de connexion, port 0 + jeton) : le client écoute
  sur le premier port libre de la plage choisie (1024 à 5000 par défaut : plusieurs transferts
  passifs simultanés ont chacun leur port). Par **UPnP** (activé par défaut), il demande à la box
  d'ouvrir ce port vers le PC et lui demande l'adresse IP publique ; sans UPnP sur la box, les
  ports doivent y être redirigés à la main. L'adresse annoncée au bot est une adresse **IPv4**
  (les bots ne font pas de DCC en IPv6), dans l'ordre : celle fixée dans les options, celle donnée
  par la box, celle de l'hôte du message de bienvenue du serveur (avant masquage) ou demandé au
  serveur. Pour cela, les serveurs IRC sont joints en IPv4 de préférence (IPv6 si le serveur n'a
  que ça). La box a 10 secondes pour répondre : l'offre part ensuite quoi qu'il arrive. La ligne
  du transfert indique l'adresse et le port annoncés, comme la fenêtre d'état, en rouge si
  c'est une adresse locale, injoignable depuis Internet. Le pare-feu Windows doit autoriser
  `irc-xdcc.exe`.
- Où s'affichent les messages, comme dans mIRC : la demande (`-> *Bot* XDCC SEND #12`) et l'ajout
  à la file dans la fenêtre où elle a été tapée ; une notice privée (bot, bot de recherche…) dans
  un canal où se trouve son auteur (la fenêtre active si c'en est un), sinon dans la fenêtre
  active ; la réception DCC (envoi reçu, UPnP, adresse du DCC passif, fin ou échec, par exemple
  « Réception DCC de fichier depuis Bot terminée (00:00:09, 556 Kio/s) ») dans la fenêtre d'état
  du serveur.
- Transfert interrompu : nouvelles tentatives automatiques (avec reprise). Bot muet trop
  longtemps, absent du réseau ou parti : demande abandonnée puis retentée (son « bot
  injoignable »). S'il finit par envoyer le pack, l'envoi est rattaché à la demande : accepté,
  affiché dans sa fenêtre, et pas redemandé.
- Fenêtre **Transferts** : la file (monter ▲, descendre ▼, annuler, relancer) et les réceptions
  (progression, débit, temps restant, durée du téléchargement ; ouvrir le fichier reçu avec le
  logiciel associé, ou l'afficher dans son dossier) ; boutons
  *Retirer les terminés* et *Ouvrir le dossier de téléchargement*. Sous Windows, un onglet de
  l'Explorateur déjà ouvert sur le dossier est réactivé au lieu d'en ouvrir un autre.

## Police

[Fixedsys Excelsior 3.02](https://github.com/kika/fixedsys) (dernière publication du dépôt, v3.09.10),
domaine public, intégrée en deux variantes qui ne diffèrent que par deux ligatures :

| Saisie | Standard (par défaut) | ALT |
|---|---|---|
| `<=` | flèche ⇐ | inférieur ou égal ≤ |
| `=<` | inférieur ou égal ≤ | (pas de ligature) |

La variante standard est choisie par défaut : dans une discussion, `<=` sert surtout de flèche
(« <= lui »), comme `=>` ; la variante ALT est pensée pour le code. Les autres ligatures
(`->`, `!=`, `>=`, `::`…) sont communes aux deux.

C'est une police « pixel » 8×16 : elle n'est nette qu'à 16 pixels d'écran ou un multiple. La
taille automatique choisit donc 16 px × l'échelle Windows arrondie (100 % → 16 px, 200 % →
32 px) ; on peut la fixer à 16, 32 ou 48 px.

mIRC utilise la Fixedsys de Windows, une police bitmap (`.fon`) qui ne peut pas être chargée
ici : Fixedsys Excelsior en est la reproduction. *Options → Affichage → Autre police* accepte
n'importe quel fichier `.ttf` / `.otf` (Consolas, Lucida Console…), avec sa taille en points.
Quelle que soit la police, un caractère qu'elle n'a pas est pris dans une police du système.
Les bitmaps intégrés à une police (y compris celles du système) sont ignorés, seul son dessin
vectoriel sert : le fichier de Fixedsys Excelsior en contient un pour « A », qui s'affichait
sinon en blanc.
