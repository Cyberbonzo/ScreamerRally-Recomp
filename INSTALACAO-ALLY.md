# Screamer Rally (1997) no ROG Ally com Bazzite

Esta adaptação parte da versão v0.1.1 de ScreamerRally-Recomp. O executável Windows de 32 bits roda pelo Proton Experimental. Para jogar, forneça sua própria cópia de Screamer Rally. Na instalação testada, o arquivo CUE aponta para o IMG do CD em inglês, com oito faixas de música.

## Compilar e instalar

1. No Windows, use o toolchain Rust MSVC com o alvo `i686-pc-windows-msvc` e as ferramentas C++ de 32 bits do Visual Studio. Na raiz deste projeto, execute `cargo build --locked --release -p screamer --no-default-features`.
2. Crie `~/Games/ScreamerRally-Recomp` no Ally. Copie `target/i686-pc-windows-msvc/release/ScreamerRally.exe` para essa pasta com o nome `ScreamerRally-Ally.exe`. Copie também `jogar.sh` e aplique `chmod +x jogar.sh`.
3. Coloque a imagem do seu CD em `disco/`, mantendo os nomes e as referências do CUE. Se quiser os filtros CRT opcionais, use o pacote de shaders da distribuição pública do projeto.
4. Com o Proton Experimental e o Steam Linux Runtime 4 instalados na Steam, adicione `jogar.sh` como jogo não Steam. Use a pasta do script como diretório de trabalho. O script chama o Proton diretamente e guarda seu prefixo em `.proton/`.
5. Na primeira abertura, selecione o CUE. Use **View** ou **F12** para configurar a apresentação. As opções ficam em `data/settings.ini`, e os dados gravados pelo jogo ficam em `data/C/SR/`.

## Imagem e controle

O perfil final usa tela cheia sem bordas, 1920×1080, aspecto 16:9, escala interna de 100%, interpolação ligada e limite de 60 FPS no Gamescope. O modo exclusivo de 1920×1080 a 60 Hz também foi testado. O cenário 3D ganha campo de visão nas laterais. Imagens 2D fixas, como a tela de carregamento, conservam a proporção original. A física está na taxa padrão do projeto, 33 passos por segundo.

| Comando | Controle do Ally |
| --- | --- |
| Dirigir | Analógico esquerdo |
| Acelerar e frear | RT e LT |
| Confirmar | A |
| Pausar ou continuar | Start |
| Configurações | View |

## Correções para o Proton Experimental

- O runtime lê o seletor FS do processo e trata os acessos à memória convencional do DOS usados na inicialização pelo WoW64.
- Com `SR_COOPERATIVE_TIMER=1`, o jogo recebe as interrupções quando espera pelo próximo quadro. Telas que continuam desenhando sem passar por essa espera recebem o tempo decorrido e a entrada do controle na apresentação do quadro. Isso mantém o menu de pausa responsivo.
- O serviço de memória DPMI respeita os 256 MiB que anuncia e contabiliza os blocos em uso. Antes, durante o carregamento da China, o jogo chegou a sondar um bloco de aproximadamente 1,7 GiB.
- O atalho desliga o rastreamento detalhado de exceções do Wine apenas neste jogo. O log normal, em `logs/runtime.log`, continua mostrando a resolução interna, o controle detectado, o temporizador e a atividade do renderizador.

## Verificação

No Ally, a versão corrigida passou pela seleção da pista, carregou Rally China e chegou à contagem regressiva. Com a corrida pausada, **A** em **Resume** devolveu o controle ao jogador. As capturas mostraram o cronômetro passar de 2,87 para 5,18 segundos e o carro avançar. O jogador confirmou o funcionamento no aparelho. O log confirmou o controle conectado, o temporizador cooperativo e a cena 3D em 1920×1080.

Para testar o runtime no Windows, execute `cargo test --locked --release -p sr-runtime --lib`. Na revisão testada, 85 testes passaram e um estava marcado para ser ignorado.
