# Generates the RPG Maker VX Ace / XP test data in this folder (run with any Ruby).
load File.join(__dir__, "make_classes.rb")

def c(code, indent, params) = RPG::EventCommand.new(code, indent, params)
shared = "共有の文字列"
list = [
  c(101, 0, ["Actor1", 0, 0, 2]),
  c(401, 0, ["こんにちは！"]),
  c(401, 0, ["\\C[2]旅人\\C[0]さん、ようこそ。"]),
  c(102, 0, [["はい", "いいえ"], 2]),
  c(402, 0, [0, "はい"]),
  c(101, 1, ["", 0, 0, 2]),
  c(401, 1, ["よかった。"]),
  c(0, 1, []),
  c(402, 0, [1, "いいえ"]),
  c(0, 1, []),
  c(404, 0, []),
  c(355, 0, ["$game_variables[1] = '" + "スクリプト" + "'"]),
  c(320, 0, [1, "ハロルド"]),
  c(108, 0, [shared]),
  c(408, 0, [shared]),
  c(0, 0, [])
]
map = RPG::Map.new("はじまりの村", { 1 => RPG::Event.new(1, [RPG::Event::Page.new(list)]) })
File.binwrite(File.join(__dir__, "Map001.rvdata2"), Marshal.dump(map))
ce = [nil, RPG::CommonEvent.new(1, "共通", [c(101, 0, ["", 0, 0, 2]), c(401, 0, ["共通イベントのメッセージ。"]), c(0, 0, [])])]
File.binwrite(File.join(__dir__, "CommonEvents.rvdata2"), Marshal.dump(ce))
File.binwrite(File.join(__dir__, "Actors.rvdata2"), Marshal.dump([nil, RPG::Actor.new("ハロルド", "", "旅の剣士。")]))
File.binwrite(File.join(__dir__, "Items.rvdata2"), Marshal.dump([nil, RPG::Item.new("ポーション", "HPを500回復する。")]))
File.binwrite(File.join(__dir__, "System.rvdata2"), Marshal.dump(RPG::System.new))
# XP: "Show Text" holds the first line itself.
xp = [nil, RPG::CommonEvent.new(1, "XP", [c(101, 0, ["一行目です。"]), c(401, 0, ["二行目です。"]), c(0, 0, [])])]
File.binwrite(File.join(__dir__, "CommonEvents.rxdata"), Marshal.dump(xp))
