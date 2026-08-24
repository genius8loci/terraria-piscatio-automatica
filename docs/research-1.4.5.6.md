# Разведка цели — Terraria 1.4.5.8

> Файл назван по версии из PE-ресурса (1.4.5.6), но на живом процессе
> `Main.versionNumber` и версия сборки дают **1.4.5.8**. Декомпилировался
> тот же самый `Terraria.exe`, так что выводы относятся к 1.4.5.8.

Все факты ниже получены из бинарника и декомпиляции установленной копии,
а не из общих знаний. Строки указаны по декомпилированному C# (ILSpy 7.2.1).

## Бинарник

| Параметр | Значение |
|---|---|
| Путь | `C:\Program Files (x86)\Steam\steamapps\common\Terraria\Terraria.exe` |
| Версия | **сборка 1.4.5.8**; ресурс FileVersion в PE отстал и показывает 1.4.5.6 |
| PE | PE32, Machine `0x014C` (i386) |
| CLR header | ILONLY \| 32BITREQUIRED → процесс **всегда 32-битный** |
| Рантайм | .NET Framework 4.x |
| Графика | XNA 4.0 → Direct3D9 (на Vista+ создаётся D3D9**Ex**) |
| Типов в сборке | 2851 |

**Следствие:** DLL собирается под `i686-pc-windows-msvc`, рендер-хук — D3D9.

## Контент

- `Content/Fonts/*.xnb` — 5 SpriteFont: `Mouse_Text`, `Item_Stack`, `Death_Text`,
  `Combat_Text`, `Combat_Crit`.
- `Content/Images/` — 13908 файлов, `Content/Images/UI/` — 210.
- Заголовок XNB: `58 4E 42 77 05 80` → платформа `w`, версия 5, флаг `0x80` = **LZX-сжатие**.
  Для чтения ассетов нужен LZX-декомпрессор (крейт `lzxd`).

## Механика рыбалки — проверено по коду

### Поля, которые нужны

| Поле | Тип | Где |
|---|---|---|
| `Projectile.bobber` | `bool` | Projectile.cs:98 — надёжный признак поплавка, проверка type не нужна |
| `Projectile.ai` | `float[]` | Projectile.cs:122 |
| `Projectile.localAI` | `float[]` | Projectile.cs:124 |
| `Player.controlUseItem` | `bool` | Player.cs:1630 |
| `Main.ThrottleWhenInactive` | `static bool` | Main.cs:2229, по умолчанию `true` |

### Состояния поплавка

Из `Projectile.AI_061_FishingBobber()` (Projectile.cs:50749):

- `ai[0] == 0` — поплавок заброшен и работает.
- `ai[0] >= 1` — идёт подтягивание к игроку (`== 2` — момент подсечки со всплеском).
- `ai[1] == 0` — простой. При этом `localAI[1]` работает как **счётчик накопления
  поклёвки**: растёт на `rand(1,3) + fishingLevel/30` за тик, при `> 660`
  сбрасывается в 0 и вызывается `FishingCheck()`.
- `ai[1] < 0` — **активная поклёвка**, окно подсечки. Значение задаётся как
  `rand(-240,-90) - fishingLevel` и растёт на `rand(1,5)` за тик.
- Когда `ai[1]` дорастает до 0 — `localAI[1] = 0`, поклёвка упущена, всплеск.

### Что клюнуло — известно ДО подсечки

`Projectile.SetFishingCheckResults()` (Projectile.cs:19331) на поклёвке пишет:

```csharp
ai[1]      = rand(-240, -90) - fishingLevel;   // окно подсечки
localAI[1] = fisher.rolledItemDrop;            // item id улова
localAI[2] = fisher.playerFishingConditions.BaitItemType;
```

а для вражеского спавна — `localAI[1] = -fisher.rolledEnemySpawn`.

**Итог: `localAI[1] > 0` — id предмета, `localAI[1] < 0` — минус id NPC.**
Это тот же источник, из которого Sonar Potion рисует всплывающий текст.
Фильтр до подсечки полностью реализуем.

### Наживка тратится только при успешной подсечке

`Player.ItemCheck_CheckFishingBobber()` (Player.cs:51530):

```csharp
if (whoAmI == Main.myPlayer && projectile.ai[0] == 0f)
{
    projectile.ai[0] = 1f;                       // начать подтягивание
    ...
    if (projectile.ai[1] < 0f && projectile.localAI[1] != 0f
        && ItemCheck_CheckFishingBobber_ConsumeBait(projectile, out var baitTypeUsed))
    {
        ItemCheck_CheckFishingBobber_PullBobber(projectile, baitTypeUsed);
    }
}
```

Наживка расходуется внутри `ConsumeBait`, и только на этом пути.
**Пропуск поклёвки наживку не тратит — фильтр бесплатный.**

Ещё из этого метода: цикл идёт по `Main.projectile[0..1000]` с условием
`active && owner == whoAmI && bobber`, и наличие поплавка запрещает новый заброс.
То есть поплавок у игрока всегда один.

### Поиск наживки в инвентаре

`Player.Fishing_GetBait()` (Player.cs:51614) — скан `inventory[i]`
на `stack > 0 && bait > 0`. Той же логикой определяем момент «наживка кончилась».

## Работа при свёрнутом окне

- `Main.ThrottleWhenInactive` (Main.cs:2229, по умолчанию `true`) при неактивном окне
  ставит `InactiveSleepTime = 20 ms` (Main.cs:16944) и включает `IsFixedTimeStep`.
  Ставим `false` через рефлексию — игра идёт полной скоростью свёрнутой.
- Реальный ввод при отсутствии фокуса игрой игнорируется → `SendInput` неприменим.
- Точка входа для инъекции ввода: **`Player.ItemCheck()`** (Player.cs:41957, `public`,
  без аргументов) — вызывается каждый кадр, внутри читает `controlUseItem`
  (Player.cs:42120) и выставляет `releaseUseItem = !controlUseItem` (Player.cs:42162).
  Детур ставится нативно по адресу из `MethodHandle.GetFunctionPointer()`.
  Клик эмулируется корректной парой кадров press/release.
- `EndScene` при свёрнутом окне не вызывается → логика рыбалки обязана жить
  в managed-детуре, а не в рендер-хуке.

## Инструменты на машине

- rustc 1.97.1, таргет `i686-pc-windows-msvc` установлен.
- dnSpy и ILSpy в `~/Downloads`; `dnSpy.Console.exe` в этом окружении нерабочий
  (падает на `Console.OutputEncoding`, `SetConsoleOutputEncoding` → invalid handle).
- Рабочий путь декомпиляции: свой мини-инструмент на пакете
  `ICSharpCode.Decompiler 7.2.1.6856` под .NET SDK 6.0.428.
