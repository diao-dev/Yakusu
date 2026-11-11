# The classes and instance variables RPG Maker uses, for make.rb and check.rb.

module RPG
  class EventCommand
    def initialize(code = 0, indent = 0, parameters = [])
      @code = code; @indent = indent; @parameters = parameters
    end
  end
  class Event
    class Page
      def initialize(list); @condition = nil; @graphic = nil; @list = list; end
    end
    def initialize(id, pages); @id = id; @name = "EV%03d" % id; @x = 1; @y = 2; @pages = pages; end
  end
  class Map
    def initialize(name, events); @display_name = name; @width = 17; @height = 13; @data = Table.new; @events = events; @note = ""; end
  end
  class CommonEvent
    def initialize(id, name, list); @id = id; @name = name; @trigger = 0; @switch_id = 1; @list = list; end
  end
  class Actor
    def initialize(name, nick, desc); @id = 1; @name = name; @nickname = nick; @description = desc; @face_name = "Actor1"; end
  end
  class Item
    def initialize(name, desc); @id = 1; @name = name; @description = desc; @icon_index = 3; end
  end
  class System
    class Terms
      def initialize; @basic = ["レベル", "Lv", "ＨＰ"]; @params = ["最大HP"]; @etypes = ["武器"]; @commands = ["戦う", "逃げる"]; end
    end
    def initialize; @game_title = "桜の旅"; @currency_unit = "G"; @elements = ["", "炎"]; @skill_types = ["", "魔法"];
      @weapon_types = [""]; @armor_types = [""]; @switches = ["", "イベント済み"]; @variables = ["", "所持金"]; @terms = Terms.new; @title1_name = "タイトル"; end
  end
end
class Table
  def initialize; @data = [1, 2, 3]; end
  def _dump(level) = [1, 3, 1, 1, 3, 1, 2, 3].pack("V5v3")
  def self._load(s) = new
end

