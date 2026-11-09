import QtQuick 2.15

Rectangle {
    id: root
    width: 200; height: 100

    function toggleColor() {
        root.color = root.color == "red" ? "blue" : "red"
    }

    MouseArea {
        anchors.fill: parent
        onClicked: root.toggleColor()
    }
}
