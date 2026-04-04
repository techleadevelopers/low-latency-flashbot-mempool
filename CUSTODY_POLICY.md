# Custody Policy

## Finalidade

Este sistema existe para proteger e custodiar fundos do próprio sistema operacional da empresa.

## Escopo

- wallets operacionais registradas em `keys.txt`
- contrato delegado da própria empresa
- tesouraria/forwarder da própria empresa

## Regras

1. O bot só pode agir sobre wallets registradas e autorizadas.
2. O destino deve ser fixo e auditável.
3. O contrato deve ser o contrato oficial da empresa na rede correspondente.
4. Toda operação deve ser observável no dashboard e persistida em storage local.
5. `shadow` é o modo padrão para validação.
6. `paper` é o modo de homologação controlada.
7. `live` só entra após validação operacional.

## Arquitetura

- monitor de custódia
- motor de reação
- política de tesouraria
- trilha de auditoria

## Critérios de produção

- contrato real configurado
- tesouraria real configurada
- latência validada
- destino auditável
- risco operacional conhecido
